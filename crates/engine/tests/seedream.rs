//! Real Seedream adapter -> no-redirect Http contract -> ProviderRunner -> scheduler Driver.
//! Synthetic local transport acceptance, not evidence of BytePlus processing or a paid request.
#![cfg(not(target_arch = "wasm32"))]
use futures::executor::block_on;
use nodecraft_engine::jobs::{JobBudget, JobPrice, ProviderRunner, QueuedJob};
use nodecraft_engine::provider_routing::RegisteredProvider;
use nodecraft_exec::cost::{Budgets, Charge, UsageLog};
use nodecraft_exec::driver::{CancelToken, JobRunner, Runtime, TaskFuture};
use nodecraft_exec::scheduler::{Event, JobId, JobSpec, JobState, Lane, Limits, Outcome, RetryPolicy, Scheduler, Start};
use nodecraft_providers::secrets::Secret;
use nodecraft_providers::seedream::{MODEL, Seedream};
use nodecraft_providers::{BoxFuture, ErrorKind, Http, HttpRequest, HttpResponse, JobInput, ProviderError, ProviderJob, RunLimits};
use nodecraft_types::{Currency, Money, NodeId};
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
        assert!(r.url.ends_with("/images/generations"));
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
fn registration(http: Arc<Transport>, price: Option<Option<Money>>) -> QueuedJob {
    QueuedJob::new(
        RegisteredProvider::default_instance(Arc::new(Seedream::new(Secret::new("fixture-key-not-real").unwrap(), http))).unwrap(),
        ProviderJob {
            model: MODEL.into(),
            params: serde_json::Map::new(),
            inputs: vec![JobInput::Text { role: "prompt".into(), text: "A blue circle".into() }],
            idempotency_key: "fixture-image-1".into(),
        },
        price.map(|estimate| JobPrice::new(NodeId::from_bytes([1; 16]), "seedream.4_5".into(), estimate)),
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
    drive_one(runner, JobId(1), "fixture-image-1")
}
fn drive_one(runner: Arc<ProviderRunner>, id: JobId, key: &str) -> (Scheduler, Vec<Event>) {
    let mut scheduler = Scheduler::new(Limits::default());
    scheduler
        .add(vec![JobSpec {
            id,
            deps: vec![],
            lane: Lane::new("seedream"),
            retry: RetryPolicy { max_attempts: 4, base_delay_ms: 1, max_delay_ms: 2 },
            idempotency_key: key.into(),
        }])
        .unwrap();
    let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
    drop(handle);
    let mut events = vec![];
    let end = block_on(driver.run(runner, Arc::new(Immediate(AtomicU64::new(0))), |event| events.push(event)));
    (end, events)
}
#[test]
fn real_seedream_adapter_never_automatically_replays_an_uncertain_submit() {
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
fn catalog_estimate_respects_each_finite_budget_before_transport() {
    let catalog = nodecraft_catalog::Catalog::builtin().unwrap();
    let quote = catalog.get("seedream.4_5").unwrap().quote(&Default::default()).unwrap().unwrap();
    assert_eq!(quote, money(40000));
    for amount in [0, 39999, 40000] {
        for scope in 0..3 {
            let mut caps = Budgets::default();
            match scope {
                0 => caps.per_call = Some(money(amount)),
                1 => caps.session = Some(money(amount)),
                _ => caps.project = Some(money(amount)),
            };
            let t = Arc::new(Transport { script: Script::Response(200, success()), requests: AtomicUsize::new(0) });
            let (r, b, o) = runner(caps);
            r.register(JobId(1), registration(t.clone(), Some(Some(quote))));
            let (s, _) = drive(r);
            assert_eq!(t.requests.load(Ordering::SeqCst), usize::from(amount >= 40000));
            assert_eq!(o.load(Ordering::SeqCst), usize::from(amount >= 40000));
            assert_eq!(matches!(s.state(JobId(1)), Some(JobState::Done)), amount >= 40000);
            if amount < 40000 {
                assert_eq!(b.log.lock().unwrap().entries().count(), 0);
            } else {
                assert_eq!(b.log.lock().unwrap().entries().next().unwrap().cost, quote);
            }
        }
    }
}
fn success() -> Vec<u8> {
    let bytes = include_bytes!("fixtures/seedream15.jpg");
    // Standard base64 encoding without an additional engine dependency.
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for c in bytes.chunks(3) {
        let a = c[0];
        let b = c.get(1).copied().unwrap_or(0);
        let d = c.get(2).copied().unwrap_or(0);
        text.push(alphabet[(a >> 2) as usize] as char);
        text.push(alphabet[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        text.push(if c.len() > 1 { alphabet[(((b & 15) << 2) | (d >> 6)) as usize] as char } else { '=' });
        text.push(if c.len() > 2 { alphabet[(d & 63) as usize] as char } else { '=' });
    }
    serde_json::to_vec(&serde_json::json!({"model":MODEL,"data":[{"size":"16x16","b64_json":text}]})).unwrap()
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
fn cancellation_before_and_during_real_seedream_transport_preserves_billing_uncertainty() {
    for pending in [false, true] {
        let t = Arc::new(Transport { script: Script::Pending, requests: AtomicUsize::new(0) });
        let (r, b, _) = runner(Budgets::default());
        r.register(JobId(1), registration(t.clone(), Some(Some(money(7)))));
        let cancel = CancelToken::default();
        if !pending {
            cancel.cancel();
        }
        let mut work = r.run(Start { job: JobId(1), lane: Lane::new("seedream"), attempt: 1, idempotency_key: "fixture-image-1".into() }, cancel.clone());
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
fn actual_adapter_completed_routes_to_scalar_run_and_original_survives_refusal() {
    use nodecraft_exec::{
        planner::{RunPlan, Step},
        run::{Run, RunLimits as ExecutionLimits, RunOptions},
    };
    use nodecraft_graph::{Canvas, Change, Port, Project};
    use nodecraft_nodes::{Category, GenerateInput, GenerateOutput, JobSpec as Work, ModeId, NodeDescriptor, NodeKind, PlanCtx};
    use nodecraft_types::{CanvasId, Modality, ParamSchema, PortType, ProjectId, Value};
    struct Kind(NodeDescriptor);
    impl NodeKind for Kind {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.0
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            ModeId::new("generate").ok()
        }
        fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<Work>, nodecraft_types::Error> {
            panic!("eager supplied jobs")
        }
    }
    let c = CanvasId::from_bytes([1; 16]);
    let n = NodeId::from_bytes([2; 16]);
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let descriptor = NodeDescriptor {
        id: "fixture.seedream".into(),
        label: "Seedream".into(),
        category: Category::Generation,
        inputs: vec![],
        outputs: vec![Port { name: "image".into(), port_type: PortType { modality: Modality::Image, many: false } }],
        params: ParamSchema {
            params: vec![nodecraft_types::ParamSpec {
                name: "model".into(),
                label: "Model".into(),
                description: None,
                control: nodecraft_types::ParamControl::Model,
                required: false,
                default: Some(Value::Text("seedream.4_5".into())),
                visible_if: None,
            }],
        },
        quick_add: None,
    };
    let instance = descriptor.instantiate(n, &Default::default()).unwrap();
    kinds.register(Box::new(Kind(descriptor))).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([3; 16]), "Seedream fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(c, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: c, node: instance }.apply(&mut project).unwrap();
    let routes = vec![GenerateOutput { port: "image".into(), modality: Modality::Image, style_family: None }];
    let work = Work::Generate {
        instance: None,
        node: n,
        provider: "seedream".into(),
        model: MODEL.into(),
        params: Default::default(),
        inputs: vec![GenerateInput { role: "prompt".into(), value: Value::Text("Blue circle".into()) }],
        outputs: routes.clone(),
    };
    let (_, request) = nodecraft_engine::generate::provider_job(&work, "fixture-run").unwrap();
    let transport = Arc::new(Transport { script: Script::Response(200, success()), requests: AtomicUsize::new(0) });
    let provider = Seedream::new(Secret::new("fixture").unwrap(), transport);
    let nodecraft_providers::Submission::Finished(original) = block_on(nodecraft_providers::Provider::submit(&provider, &request)).unwrap() else { panic!() };
    let preserved = original.clone();
    assert!(nodecraft_engine::generate::produced(&[GenerateOutput { port: "text".into(), modality: Modality::Text, style_family: None }], &original).is_err());
    assert_eq!(original, preserved);
    let converted = nodecraft_engine::generate::produced(&routes, &original).unwrap();
    assert_eq!(converted.assets.len(), 1);
    let plan = RunPlan {
        steps: vec![Step {
            node: n,
            kind: "fixture.seedream".into(),
            mode: ModeId::new("generate").unwrap(),
            after: vec![],
            inputs: vec![],
            jobs: Some(vec![work.clone()]),
            fanout: None,
            variations: 1,
        }],
        edges: vec![],
        inner: Default::default(),
    };
    let options = RunOptions { id: [4; 16], first_job: 1, limits: ExecutionLimits::default(), retry: RetryPolicy::NEVER };
    let canvas = project.canvases().get(&c).unwrap();
    let mut targets = nodecraft_types::TargetSelection::empty();
    targets.insert(n, &nodecraft_types::ProviderTarget::default_instance("seedream").unwrap()).unwrap();
    let catalog = nodecraft_catalog::Catalog::builtin().unwrap();
    let (admitted, quote) = nodecraft_exec::admission::admit_run_with_targets(plan, c, canvas.clone(), &kinds, &catalog, Default::default(), &targets).unwrap();
    assert_eq!(quote.steps.len(), 1);
    let (mut run, progress) = Run::start_admitted(admitted, &kinds, options).unwrap();
    assert!(progress.failed.is_empty());
    assert_eq!(progress.jobs.len(), 1);
    let finished = run.job_done(&kinds, progress.jobs[0].id, converted.outputs);
    assert_eq!(finished.finished.len(), 1);
    assert!(run.is_finished());
    assert_eq!(original, preserved);
    let Work::Generate { mut inputs, .. } = work.clone() else { panic!() };
    inputs.push(GenerateInput { role: "image".into(), value: finished.finished[0].generation.outputs[0].clone() });
    let Work::Generate { node, provider, instance, model, params, outputs, .. } = work else { panic!() };
    let edit = Work::Generate { node, provider, instance, model, params, inputs, outputs };
    assert!(nodecraft_engine::generate::provider_job(&edit, "edit").is_err(), "existing canvas resolver limitation stays explicit");
}

#[test]
fn budget_change_after_registration_is_rechecked_before_paid_submit() {
    let t = Arc::new(Transport { script: Script::Response(200, success()), requests: AtomicUsize::new(0) });
    let (r, b, o) = runner(Budgets::default());
    r.register(JobId(1), registration(t.clone(), Some(Some(money(40000)))));
    b.budgets.lock().unwrap().session = Some(money(0));
    let (s, _) = drive(r);
    assert!(matches!(s.state(JobId(1)), Some(JobState::Failed { .. })));
    assert_eq!(t.requests.load(Ordering::SeqCst), 0);
    assert_eq!(o.load(Ordering::SeqCst), 0);
    assert_eq!(b.log.lock().unwrap().entries().count(), 0);
}

#[test]
fn local_empty_prompt_refusal_has_zero_transport_and_zero_usage() {
    let t = Arc::new(Transport { script: Script::Response(200, success()), requests: AtomicUsize::new(0) });
    let (r, b, outputs) = runner(Budgets { session: Some(money(40000)), ..Default::default() });
    let mut queued = registration(t.clone(), Some(Some(money(40000))));
    queued.job.inputs = vec![JobInput::Text { role: "prompt".into(), text: String::new() }];
    r.register(JobId(1), queued);
    let (scheduler, events) = drive(r);
    assert!(matches!(scheduler.state(JobId(1)), Some(JobState::Failed { .. })));
    assert_eq!(t.requests.load(Ordering::SeqCst), 0);
    assert_eq!(outputs.load(Ordering::SeqCst), 0);
    assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })));
    let charges = b.log.lock().unwrap().entries().map(|e| (e.cost, e.status)).collect::<Vec<_>>();
    assert!(charges.is_empty(), "a purely local refusal must release its reservation");
}

#[test]
fn every_local_refusal_releases_the_same_finite_allowance_for_valid_work() {
    use nodecraft_providers::seedream::Limits as ProviderLimits;
    for case in 0..6 {
        let t = Arc::new(Transport { script: Script::Response(200, success()), requests: AtomicUsize::new(0) });
        let (r, budget, outputs) = runner(Budgets { session: Some(money(40000)), project: Some(money(40000)), ..Default::default() });
        let mut queued = registration(t.clone(), Some(Some(money(40000))));
        let mut limits = ProviderLimits::HARD;
        let media = || JobInput::Media { role: "image".into(), mime: "image/jpeg".into(), bytes: Arc::from(&include_bytes!("fixtures/seedream15.jpg")[..]) };
        match case {
            0 => queued.job.inputs = vec![JobInput::Text { role: "prompt".into(), text: String::new() }],
            1 => {
                queued.job.params.insert("unknown".into(), serde_json::json!(true));
            }
            2 => {
                queued.job.inputs.push(media());
                queued.job.inputs.push(JobInput::Media { role: "image".into(), mime: "image/jpeg".into(), bytes: Arc::from(&b"corrupt last reference"[..]) });
            }
            3 => {
                let mut input = media();
                let JobInput::Media { mime, .. } = &mut input else { panic!() };
                *mime = "image/png".into();
                queued.job.inputs.push(input);
            }
            4 => limits.request_bytes = 1,
            5 => limits.axis -= 1,
            _ => unreachable!(),
        }
        queued.provider =
            RegisteredProvider::default_instance(Arc::new(Seedream::with_limits(Secret::new("fixture").unwrap(), t.clone(), limits).unwrap())).unwrap();
        r.register(JobId(1), queued);
        let (failed, events) = drive(r.clone());
        assert!(matches!(failed.state(JobId(1)), Some(JobState::Failed { .. })), "case {case}");
        assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })));
        assert_eq!(t.requests.load(Ordering::SeqCst), 0, "case {case}");
        assert_eq!(outputs.load(Ordering::SeqCst), 0);
        assert_eq!(budget.log.lock().unwrap().entries().count(), 0, "case {case}");
        let mut valid = registration(t.clone(), Some(Some(money(40000))));
        valid.job.idempotency_key = "fixture-image-2".into();
        r.register(JobId(2), valid);
        let (success, _) = drive_one(r, JobId(2), "fixture-image-2");
        assert!(matches!(success.state(JobId(2)), Some(JobState::Done)), "released allowance case {case}");
        assert_eq!(t.requests.load(Ordering::SeqCst), 1);
        assert_eq!(outputs.load(Ordering::SeqCst), 1);
        assert_eq!(budget.log.lock().unwrap().entries().map(|e| e.cost).collect::<Vec<_>>(), [money(40000)]);
    }
}
