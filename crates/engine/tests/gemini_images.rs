//! Real Images adapter -> no-redirect Http contract -> ProviderRunner -> scheduler Driver.
//! Synthetic local transport acceptance, not evidence of Google processing or a paid request.
#![cfg(not(target_arch = "wasm32"))]
use futures::executor::block_on;
use nodecraft_engine::jobs::{JobBudget, JobPrice, ProviderRunner, QueuedJob};
use nodecraft_engine::provider_routing::RegisteredProvider;
use nodecraft_exec::cost::{Budgets, Charge, UsageLog};
use nodecraft_exec::driver::{CancelToken, JobRunner, Runtime, TaskFuture};
use nodecraft_exec::scheduler::{Event, JobId, JobSpec, JobState, Lane, Limits, Outcome, RetryPolicy, Scheduler, Start};
use nodecraft_providers::gemini_images::{GeminiImages, LITE};
use nodecraft_providers::secrets::Secret;
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpRequest, HttpResponse, JobInput, ProviderError, ProviderJob, RunLimits};
use nodecraft_types::{Currency, Money, NodeId, ProviderTarget};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
struct Immediate(AtomicU64);
impl Runtime for Immediate {
    fn spawn(&self, mut task: TaskFuture<()>) {
        let w = futures::task::noop_waker();
        assert!(task.as_mut().poll(&mut std::task::Context::from_waker(&w)).is_ready());
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        self.0.fetch_add(ms, Ordering::SeqCst);
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
enum Script {
    Lost(ErrorKind),
    Response(u16, Vec<u8>),
    Pending,
}
struct Transport {
    script: Script,
    requests: AtomicUsize,
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("Images must opt in to no redirect")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        assert!(r.url.ends_with("/interactions"));
        assert!(!r.body.is_empty());
        self.requests.fetch_add(1, Ordering::SeqCst);
        match &self.script {
            Script::Lost(kind) => {
                let kind = *kind;
                Box::pin(async move { Err(ProviderError::new(kind, "fixture accepted request, then lost reply")) })
            }
            Script::Response(status, body) => {
                let r = HttpResponse { status: *status, headers: vec![], body: body.clone() };
                Box::pin(async move { Ok(r) })
            }
            Script::Pending => Box::pin(futures::future::pending()),
        }
    }
}
fn money(n: u64) -> Money {
    Money::new(Currency::USD, n)
}
fn lane() -> Lane {
    Lane::new(ProviderTarget::default_instance("gemini-images").unwrap().lane_name().unwrap())
}
fn registration(http: Arc<Transport>, price: Option<Option<Money>>) -> QueuedJob {
    let provider = RegisteredProvider::default_instance(Arc::new(GeminiImages::new(Secret::new("fixture-key-not-real").unwrap(), http))).unwrap();
    assert_eq!(provider.target().adapter(), "gemini-images");
    assert_eq!(provider.target().instance(), None);
    QueuedJob::new(
        provider,
        ProviderJob {
            model: LITE.into(),
            params: serde_json::Map::new(),
            inputs: vec![JobInput::Text { role: "prompt".into(), text: "A blue circle".into() }],
            idempotency_key: "fixture-image-1".into(),
        },
        price.map(|estimate| JobPrice::new(NodeId::from_bytes([1; 16]), "gemini-images.flash-lite".into(), estimate)),
    )
}
fn runner(budgets: Budgets) -> (Arc<ProviderRunner>, JobBudget, Arc<AtomicUsize>) {
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(budgets)),
        session: "fixture".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let outputs = Arc::new(AtomicUsize::new(0));
    let copied = outputs.clone();
    let runner = ProviderRunner::new(
        Arc::new(Immediate(AtomicU64::new(0))),
        RunLimits::default(),
        Arc::new(move |_, _| {
            copied.fetch_add(1, Ordering::SeqCst);
        }),
        Arc::new(|_, _, _| {}),
    )
    .with_budget(budget.clone());
    (Arc::new(runner), budget, outputs)
}
fn drive(runner: Arc<ProviderRunner>) -> (Scheduler, Vec<Event>) {
    let mut scheduler = Scheduler::new(Limits::default());
    scheduler
        .add(vec![JobSpec {
            id: JobId(1),
            deps: vec![],
            lane: lane(),
            retry: RetryPolicy { max_attempts: 4, base_delay_ms: 1, max_delay_ms: 2 },
            idempotency_key: "fixture-image-1".into(),
        }])
        .unwrap();
    let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
    drop(handle);
    let mut events = vec![];
    let end = block_on(driver.run(runner, Arc::new(Immediate(AtomicU64::new(0))), |event| events.push(event)));
    (end, events)
}
#[test]
fn real_image_adapter_never_automatically_replays_an_uncertain_submit() {
    let mut scripts = vec![Script::Lost(ErrorKind::Network), Script::Lost(ErrorKind::Timeout), Script::Lost(ErrorKind::Server)];
    scripts.extend(
        [
            (429, br#"{"error":{"code":"slow_down","type":"rate_limit_error"}}"#.to_vec()),
            (503, br#"{"error":{"code":"server_is_overloaded"}}"#.to_vec()),
            (200, br#"{"data":[{"b64_json":"corrupt"}]}"#.to_vec()),
        ]
        .into_iter()
        .map(|(status, body)| Script::Response(status, body)),
    );
    for script in scripts {
        let transport = Arc::new(Transport { script, requests: AtomicUsize::new(0) });
        let (runner, budget, outputs) = runner(Budgets::default());
        runner.register(JobId(1), registration(transport.clone(), Some(Some(money(100_000)))));
        let (scheduler, events) = drive(runner);
        assert!(matches!(scheduler.state(JobId(1)), Some(JobState::Failed { .. })));
        assert_eq!(transport.requests.load(Ordering::SeqCst), 1);
        assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })));
        assert_eq!(outputs.load(Ordering::SeqCst), 0);
        assert_eq!(budget.log.lock().unwrap().entries().map(|e| (e.cost, e.status)).collect::<Vec<_>>(), [(money(100_000), Charge::Unresolved)]);
    }
}
#[test]
fn unknown_paid_price_is_refused_for_every_finite_cap_including_zero() {
    for amount in [0, 1_000_000] {
        for scope in 0..3 {
            let mut caps = Budgets::default();
            match scope {
                0 => caps.per_call = Some(money(amount)),
                1 => caps.session = Some(money(amount)),
                _ => caps.project = Some(money(amount)),
            };
            let t = Arc::new(Transport { script: Script::Lost(ErrorKind::Network), requests: AtomicUsize::new(0) });
            let (r, b, _) = runner(caps);
            r.register(JobId(1), registration(t.clone(), Some(None)));
            let (_, events) = drive(r);
            assert_eq!(t.requests.load(Ordering::SeqCst), 0);
            assert_eq!(b.log.lock().unwrap().entries().count(), 0);
            assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })));
        }
    }
    // price=None is the trusted host's local/free bypass, not an unknown paid registration.
    let t = Arc::new(Transport { script: Script::Lost(ErrorKind::Network), requests: AtomicUsize::new(0) });
    let (r, b, _) = runner(Budgets { per_call: Some(money(0)), ..Budgets::default() });
    r.register(JobId(1), registration(t.clone(), None));
    drive(r);
    assert_eq!(t.requests.load(Ordering::SeqCst), 1);
    assert_eq!(b.log.lock().unwrap().entries().count(), 0);
}
fn success() -> Vec<u8> {
    include_bytes!("../../providers/tests/fixtures/gemini_images/success.json").to_vec()
}
#[test]
fn uncapped_unknown_price_delivers_real_output_without_fabricating_money() {
    let t = Arc::new(Transport { script: Script::Response(200, success()), requests: AtomicUsize::new(0) });
    let (r, b, out) = runner(Budgets::default());
    r.register(JobId(1), registration(t.clone(), Some(None)));
    let (s, _) = drive(r);
    assert!(matches!(s.state(JobId(1)), Some(JobState::Done)));
    assert_eq!(out.load(Ordering::SeqCst), 1);
    assert_eq!(t.requests.load(Ordering::SeqCst), 1);
    assert_eq!(b.log.lock().unwrap().entries().count(), 0);
}
#[test]
fn cancellation_before_and_during_real_image_transport_preserves_billing_uncertainty() {
    for pending in [false, true] {
        let t = Arc::new(Transport { script: Script::Pending, requests: AtomicUsize::new(0) });
        let (r, b, _) = runner(Budgets::default());
        r.register(JobId(1), registration(t.clone(), Some(Some(money(7)))));
        let cancel = CancelToken::default();
        if !pending {
            cancel.cancel();
        }
        let mut work = r.run(Start { job: JobId(1), lane: lane(), attempt: 1, idempotency_key: "fixture-image-1".into() }, cancel.clone());
        if pending {
            let w = futures::task::noop_waker();
            assert!(work.as_mut().poll(&mut std::task::Context::from_waker(&w)).is_pending());
            cancel.cancel();
        }
        assert_eq!(block_on(work), Outcome::Cancelled);
        assert_eq!(t.requests.load(Ordering::SeqCst), usize::from(pending));
        let statuses = b.log.lock().unwrap().entries().map(|e| e.status).collect::<Vec<_>>();
        assert_eq!(statuses, if pending { vec![Charge::Unresolved] } else { vec![] });
    }
}

#[test]
fn pure_local_refusal_has_no_execute_and_releases_estimate() {
    let t = Arc::new(Transport { script: Script::Lost(ErrorKind::Network), requests: AtomicUsize::new(0) });
    let (r, b, _) = runner(Budgets::default());
    let mut q = registration(t.clone(), Some(Some(money(7))));
    q.job.params.insert("mask".into(), serde_json::json!(true));
    r.register(JobId(1), q);
    let (_, events) = drive(r);
    assert_eq!(t.requests.load(Ordering::SeqCst), 0);
    assert_eq!(b.log.lock().unwrap().entries().count(), 0);
    assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })));
}

#[test]
fn factory_routing_preserves_unknown_price_and_explicit_consent_before_network() {
    use nodecraft_engine::{
        Session,
        commands::{Registry as Commands, Resources, Services},
        provider_setup,
        run_cmds::{BudgetState, PreparedRun, RunCatalog, UsageSnapshot},
        secrets::{Account, MemoryStore, SecretStore},
        settings::ProviderSettings,
    };
    use nodecraft_graph::{Canvas, Change, Port, Project};
    use nodecraft_nodes::{Category, GenerateInput, GenerateOutput, JobSpec as Work, ModeId, NodeDescriptor, NodeKind, PlanCtx};
    use nodecraft_types::{CanvasId, Modality, ParamControl, ParamSchema, ParamSpec, PortType, ProjectId, Value};
    use serde_json::json;
    struct QuoteKind(NodeDescriptor);
    impl NodeKind for QuoteKind {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            ModeId::new("fixture").ok()
        }
        fn plan(&self, context: &PlanCtx<'_>) -> Result<Vec<Work>, nodecraft_types::Error> {
            Ok(vec![Work::Generate {
                node: context.node.id,
                outputs: vec![GenerateOutput { port: "image".into(), modality: Modality::Image }],
                provider: "gemini-images".into(),
                instance: None,
                model: LITE.into(),
                params: Default::default(),
                inputs: vec![GenerateInput { role: "prompt".into(), value: Value::Text("Public square".into()) }],
            }])
        }
    }
    let canvas = CanvasId::from_bytes([1; 16]);
    let node = NodeId::from_bytes([2; 16]);
    for capped in [false, true] {
        let descriptor = NodeDescriptor {
            id: "fixture.gemini".into(),
            label: "Gemini fixture".into(),
            category: Category::Generation,
            inputs: vec![],
            outputs: vec![Port { name: "image".into(), port_type: PortType { modality: Modality::Image, many: false } }],
            params: ParamSchema {
                params: vec![ParamSpec {
                    name: "model".into(),
                    label: "Model".into(),
                    description: None,
                    control: ParamControl::Model,
                    required: true,
                    default: None,
                    visible_if: None,
                }],
            },
            quick_add: None,
        };
        let instance = descriptor.instantiate(node, &[("model".into(), Value::Text("gemini-images.flash-lite".into()))].into()).unwrap();
        let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
        kinds.register(Box::new(QuoteKind(descriptor))).unwrap();
        let mut project = Project::new(ProjectId::from_bytes([3; 16]), "Gemini fixture").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        Change::AddNode { canvas, node: instance }.apply(&mut project).unwrap();
        let mut resources = Resources::default();
        resources.insert(RunCatalog::new(nodecraft_catalog::Catalog::builtin().unwrap()).unwrap());
        resources.insert(BudgetState {
            budgets: if capped { Budgets { per_call: Some(money(1_000_000)), ..Default::default() } } else { Budgets::default() },
            spent: Default::default(),
        });
        resources.insert(UsageSnapshot(Arc::new(UsageLog::default())));
        let captured: Arc<Mutex<Vec<PreparedRun>>> = Arc::default();
        let saved = captured.clone();
        let mut services = Services::default();
        services
            .register_run_host(move |prepared| {
                saved.lock().unwrap().push(prepared);
                Ok(json!({"accepted":true}))
            })
            .unwrap();
        let mut session = Session::with_registries(project, Commands::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
        let keys = MemoryStore::default();
        keys.set(&Account::new("gemini").unwrap(), &Secret::new("fixture-key-not-real").unwrap()).unwrap();
        let http = Arc::new(Transport { script: Script::Pending, requests: AtomicUsize::new(0) });
        let settings = ProviderSettings::default();
        let env = provider_setup::HostEnv { keys: &keys, http: http.clone(), settings: &settings, now_ms: Arc::new(|| 0) };
        let setup = provider_setup::build(&nodecraft_catalog::Catalog::builtin().unwrap(), |adapter| provider_setup::make(adapter, &env));
        assert_eq!(setup.readiness("gemini-images"), Some(&provider_setup::Readiness::Ready));
        session.set_provider_routing(setup.routing([(node, "gemini-images")]).unwrap()).unwrap();
        let request = json!({"canvas":canvas,"mode":"selected","nodes":[node]});
        let quote = session.dispatch("run.quote", &request).unwrap();
        assert_eq!(quote["complete"], false);
        let mut start = request.clone();
        start["accept"] = json!({"token":quote["token"],"amounts":{},"partial":false,"unpriced_jobs":1});
        assert!(session.dispatch("run.start", &start).is_err());
        start["accept"]["partial"] = json!(true);
        start["accept"].as_object_mut().unwrap().remove("unpriced_jobs");
        assert!(session.dispatch("run.start", &start).is_err());
        start["accept"]["unpriced_jobs"] = json!(1);
        if capped {
            assert!(session.dispatch("run.start", &start).is_err());
            assert!(captured.lock().unwrap().is_empty());
        } else {
            session.dispatch("run.start", &start).unwrap();
            let prepared = captured.lock().unwrap().pop().unwrap();
            let options = nodecraft_exec::run::RunOptions { id: [8; 16], first_job: 1, limits: Default::default(), retry: RetryPolicy::NEVER };
            let (run, progress) = session.authorize_prepared_run(prepared, options).unwrap();
            let spec = &progress.jobs[0];
            let queued = run.queued_job(spec.id, &spec.idempotency_key).unwrap();
            assert!(queued.is_authorized());
            assert_eq!(queued.charged_price().unwrap().estimate, None);
            assert_eq!(run.unpriced_jobs(), (1, 0));
        }
        assert_eq!(http.requests.load(Ordering::SeqCst), 0);
    }
}
