//! Actual typed host seam: quote/start -> PreparedRun/authorization -> Driver/ProviderRunner -> record.
//! Media inputs go through the production conversion over a one-asset test media source.
#![cfg(not(target_arch = "wasm32"))]
use futures::executor::block_on;
use nodecraft_engine::{
    Session,
    commands::{Registry as Commands, Resources, Services},
    generate,
    jobs::{CompletionDelivery, JobBudget, JobPrice, ProviderRunner, QueuedJob},
    provider_routing::{ProviderRouting, RegisteredProvider},
    run_cmds::{BudgetState, PreparedRun, RunCatalog, UsageSnapshot},
};
use nodecraft_exec::{
    cost::{Budgets, Charge, Spent, UsageLog},
    driver::{CancelToken, JobRunner, Runtime, TaskFuture},
    run::RunOptions,
    scheduler::{self, Event, JobId, JobState, RetryPolicy, Scheduler},
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::{Category, GenerateInput, GenerateOutput, JobSpec, ModeId, NodeDescriptor, NodeKind, ParamControl, ParamSchema, ParamSpec, PlanCtx};
use nodecraft_providers::{
    BoxFuture, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, JobInput, ProviderError, ProviderJob, RunLimits,
    fal::{ESRGAN, FLUX, Fal, KOKORO, TRELLIS, WAN},
    secrets::Secret,
};
use nodecraft_types::{AssetRef, Blake3, CanvasId, Currency, Modality, Money, NodeId, PortType, ProjectId, Value as V};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);
fn usd(n: u64) -> Money {
    Money::new(Currency::USD, n)
}
fn png() -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[140; 16], &Default::default()).unwrap()
}
/// The fixture's one input image, served by hash like the session's asset store would.
struct OneAsset(Arc<[u8]>);
impl generate::MediaSource for OneAsset {
    fn size(&self, asset: &AssetRef) -> Result<u64, nodecraft_types::Error> {
        if asset.hash == hash(&self.0) {
            Ok(self.0.len() as u64)
        } else {
            Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::NotFound, "no such asset"))
        }
    }
    fn read(&self, asset: &AssetRef, limit: u64) -> Result<Arc<[u8]>, nodecraft_types::Error> {
        assert!(self.size(asset)? <= limit);
        Ok(self.0.clone())
    }
}
fn hash(b: &[u8]) -> Blake3 {
    Blake3::from_bytes(*blake3::hash(b).as_bytes())
}
struct Kind {
    d: NodeDescriptor,
    remote: &'static str,
    output: Modality,
}
impl NodeKind for Kind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.d
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("generate").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, nodecraft_types::Error> {
        let inputs = if [TRELLIS, ESRGAN].contains(&self.remote) {
            vec![GenerateInput { role: "image".into(), value: ctx.node.params.get("reference").unwrap().clone() }]
        } else {
            vec![GenerateInput { role: "prompt".into(), value: V::Text("A blue circle".into()) }]
        };
        Ok(vec![JobSpec::Generate {
            node: ctx.node.id,
            outputs: vec![GenerateOutput { port: "out".into(), modality: self.output, style_family: None }],
            provider: "fal".into(),
            instance: None,
            model: self.remote.into(),
            params: if self.remote == FLUX { [("num_images".into(), V::Unsigned(nodecraft_types::Unsigned(4)))].into() } else { Default::default() },
            inputs,
        }])
    }
}
fn catalog_id(m: &str) -> &'static str {
    match m {
        FLUX => "fal.flux-schnell",
        WAN => "fal.wan22",
        KOKORO => "fal.kokoro",
        TRELLIS => "fal.trellis2",
        ESRGAN => "fal.esrgan",
        _ => panic!(),
    }
}
fn modality(m: &str) -> Modality {
    match m {
        FLUX | ESRGAN => Modality::Image,
        WAN => Modality::Video,
        KOKORO => Modality::Audio,
        TRELLIS => Modality::Mesh,
        _ => panic!(),
    }
}
type Fixture = (Session, Arc<Mutex<Vec<PreparedRun>>>, Arc<[u8]>, Arc<Transport>);

fn fixture(m: &'static str, caps: Budgets) -> Fixture {
    let bytes: Arc<[u8]> = png().into();
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let spec =
        |name: &str, control| ParamSpec { name: name.into(), label: name.into(), description: None, control, required: false, default: None, visible_if: None };
    let mut params = vec![spec("model", ParamControl::Model)];
    if m == FLUX {
        let catalog = nodecraft_catalog::Catalog::builtin().unwrap();
        params.push(catalog.get(catalog_id(m)).unwrap().params.params.iter().find(|p| p.name == "num_images").unwrap().clone());
    }
    if [TRELLIS, ESRGAN].contains(&m) {
        params.push(spec("reference", ParamControl::Asset { modality: Modality::Image }));
    }
    kinds
        .register(Box::new(Kind {
            d: NodeDescriptor {
                id: "fixture.fal".into(),
                label: "Fal fixture".into(),
                category: Category::Generation,
                inputs: vec![],
                outputs: vec![nodecraft_graph::Port { name: "out".into(), port_type: PortType { modality: modality(m), many: m == FLUX } }],
                params: ParamSchema { params },
                quick_add: None,
            },
            remote: m,
            output: modality(m),
        }))
        .unwrap();
    let mut project = Project::new(ProjectId::from_bytes([3; 16]), "Fal fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut values = BTreeMap::from([("model".into(), V::Text(catalog_id(m).into()))]);
    if m == FLUX {
        values.insert("num_images".into(), V::Unsigned(nodecraft_types::Unsigned(4)));
    }
    if [TRELLIS, ESRGAN].contains(&m) {
        values.insert("reference".into(), V::Media(AssetRef { hash: hash(&bytes), mime: "image/png".parse().unwrap(), meta: Default::default() }));
    }
    let node = kinds.get("fixture.fal").unwrap().descriptor().instantiate(N, &values).unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(RunCatalog::new(nodecraft_catalog::Catalog::builtin().unwrap()).unwrap());
    resources.insert(BudgetState { budgets: caps, spent: Spent::default() });
    resources.insert(UsageSnapshot(Arc::new(UsageLog::default())));
    let captured: Arc<Mutex<Vec<PreparedRun>>> = Arc::default();
    let log = captured.clone();
    let mut services = Services::default();
    services
        .register_run_host(move |prepared| {
            let mut rows = log.lock().unwrap();
            assert!(rows.is_empty(), "one-shot fixture service");
            rows.push(prepared);
            Ok(json!({"accepted":true}))
        })
        .unwrap();
    let transport = Arc::new(Transport { model: m, calls: AtomicUsize::new(0), script: Script::Good });
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Fal::new(Secret::new("fixture-key-not-real").unwrap(), transport.clone()))).unwrap();
    routing.select(N, "fal", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Commands::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    (session, captured, bytes, transport)
}
fn request() -> serde_json::Value {
    json!({"canvas":C,"mode":"selected","nodes":[N]})
}
fn consent(s: &mut Session, partial: bool, amount: u64) -> serde_json::Value {
    let q = s.dispatch("run.quote", &request()).unwrap();
    let mut v = request();
    v["accept"] = json!({"token":q["token"],"partial":partial,"unpriced_jobs":1,"amounts":{"USD":amount}});
    v
}
#[derive(Default)]
struct Immediate(Arc<std::sync::atomic::AtomicU64>);
impl Runtime for Immediate {
    fn spawn(&self, mut task: TaskFuture<()>) {
        let w = futures::task::noop_waker();
        assert!(task.as_mut().poll(&mut std::task::Context::from_waker(&w)).is_ready());
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let clock = self.0.clone();
        Box::pin(async move {
            clock.fetch_add(ms, Ordering::SeqCst);
        })
    }
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
enum Script {
    Resume,
    Good,
    Lost,
    Pending,
}
struct Transport {
    model: &'static str,
    calls: AtomicUsize,
    script: Script,
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable path")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            assert_eq!(r.method, HttpMethod::Post);
        } else {
            assert_eq!(r.method, HttpMethod::Get);
        }
        if matches!(self.script, Script::Pending) {
            return Box::pin(futures::future::pending());
        }
        if matches!(self.script, Script::Lost) {
            return Box::pin(async { Err(ProviderError::new(ErrorKind::Network, "lost reply")) });
        }
        let n = if matches!(self.script, Script::Resume) {
            if n == 1 {
                return Box::pin(async { Err(ProviderError::new(ErrorKind::Network, "lost poll")) });
            }
            if n > 1 { n - 1 } else { n }
        } else {
            n
        };
        let root = match self.model {
            FLUX => "fal-ai/flux",
            WAN => "fal-ai/wan",
            KOKORO => "fal-ai/kokoro",
            m => m,
        };
        let root = format!("https://queue.fal.run/{root}/requests/id1");
        let content = if n == 0 {
            assert_eq!(r.url, format!("https://queue.fal.run/{}", self.model));
            json!({"request_id":"id1","status":"IN_QUEUE","status_url":format!("{root}/status"),"response_url":root,"cancel_url":format!("{root}/cancel")})
        } else if n == 1 {
            assert_eq!(r.url, format!("{root}/status"));
            json!({"status":"COMPLETED"})
        } else if n == 2 {
            assert_eq!(r.url, root);
            let file = json!({"url":"https://fal.media/fixture"});
            match self.model {
                FLUX => json!({"images":vec![json!({"url":"https://fal.media/fixture","width":2,"height":2});4]}),
                WAN => json!({"video":file}),
                KOKORO => json!({"audio":file}),
                TRELLIS => json!({"model_glb":file}),
                ESRGAN => json!({"image":file}),
                _ => panic!(),
            }
        } else {
            assert!(r.headers.is_empty());
            assert_eq!(r.url, "https://fal.media/fixture");
            let b = match self.model {
                FLUX => png(),
                ESRGAN => nodecraft_media::encode_png(4, 4, &[140; 64], &Default::default()).unwrap(),
                WAN => include_bytes!("../../providers/src/fal/fixtures/blue.mp4").to_vec(),
                KOKORO => include_bytes!("../../providers/src/fal/fixtures/tone.wav").to_vec(),
                TRELLIS => include_bytes!("../../providers/src/fal/fixtures/triangle.glb").to_vec(),
                _ => panic!(),
            };
            return Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body: b }) });
        };
        let b = serde_json::to_vec(&content).unwrap();
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body: b }) })
    }
}
fn budget(caps: Budgets) -> JobBudget {
    JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(caps)),
        session: "fal-fixture".into(),
        wall_clock_ms: Arc::new(|| 0),
    }
}
#[test]
fn all_five_genuine_consent_run_driver_converter_record_and_undo() {
    let runtime = Arc::new(Immediate::default());
    for m in [FLUX, WAN, KOKORO, TRELLIS, ESRGAN] {
        let (mut session, captured, input, transport) = fixture(m, Budgets::default());
        let accepted = consent(&mut session, m != TRELLIS, 250_000);
        let revision = session.revision();
        session.dispatch("run.start", &accepted).unwrap();
        assert_eq!(session.revision(), revision);
        assert_eq!(captured.lock().unwrap().len(), 1);
        let prepared = captured.lock().unwrap().pop().unwrap();
        assert!(captured.lock().unwrap().is_empty());
        assert_eq!(prepared.canvas_id(), C);
        assert_eq!(prepared.quote().steps.len(), 1);
        let step = &prepared.quote().steps[0];
        assert_eq!(step.node, N);
        assert_eq!(step.models, [catalog_id(m)]);
        assert!(!matches!(step.per_job, Ok(None)), "no Fal model is free");
        let estimate = step.per_job.as_ref().ok().copied().flatten();
        assert_eq!(estimate, if m == TRELLIS { Some(usd(250_000)) } else { None });
        let (mut run, progress) = session
            .authorize_prepared_run(
                prepared,
                RunOptions { id: [9; 16], first_job: 1, limits: Default::default(), retry: RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 1 } },
            )
            .unwrap();
        assert_eq!(progress.jobs.len(), 1);
        let spec = &progress.jobs[0];
        let work = run.work(spec.id).unwrap();
        let queued = if [TRELLIS, ESRGAN].contains(&m) {
            assert!(run.queued_job(spec.id, &spec.idempotency_key).is_err(), "media needs a media source");
            assert!(generate::provider_job(work, &spec.idempotency_key).is_err());
            let JobSpec::Generate { inputs, model, provider, instance, .. } = work else { panic!() };
            assert_eq!(provider, "fal");
            assert_eq!(*instance, None);
            assert_eq!(model, m);
            assert_eq!(inputs.len(), 1);
            assert_eq!(inputs[0].role, "image");
            let V::Media(asset) = &inputs[0].value else { panic!() };
            assert_eq!(asset.hash, hash(&input));
            assert_eq!(asset.mime.as_str(), "image/png");
            assert!(input.len() < 1024);
            // The production conversion (#502), over this fixture's one stored image.
            let queued = run.queued_job_with_media(spec.id, &spec.idempotency_key, &OneAsset(input.clone())).unwrap();
            assert_eq!(queued.job.inputs, [JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: input.clone() }]);
            queued
        } else {
            run.queued_job(spec.id, &spec.idempotency_key).unwrap()
        };
        assert!(queued.provider.same_binding(run.binding(spec.id).unwrap()));
        assert_eq!(queued.provider.target().adapter(), "fal");
        assert_eq!(queued.price.as_ref().unwrap().estimate, estimate);
        assert_eq!(queued.job.model, m);
        assert_eq!(queued.job.idempotency_key, spec.idempotency_key);
        let deliveries: Arc<Mutex<BTreeMap<JobId, CompletionDelivery>>> = Arc::default();
        let saved = deliveries.clone();
        let b = budget(Budgets::default());
        let runner = Arc::new(
            ProviderRunner::new(
                runtime.clone(),
                RunLimits::default(),
                Arc::new(move |id, delivery| {
                    assert!(saved.lock().unwrap().insert(id, delivery).is_none());
                }),
                Arc::new(|_, _, _| {}),
            )
            .with_budget(b.clone()),
        );
        runner.register(spec.id, queued);
        let mut scheduler = Scheduler::new(Default::default());
        scheduler.add(progress.jobs.clone()).unwrap();
        let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
        drop(handle);
        let mut events = vec![];
        let end = block_on(driver.run(runner, runtime.clone(), |event| events.push(event)));
        assert_eq!(end.state(spec.id), Some(&JobState::Done));
        assert!(events.contains(&Event::Done { job: spec.id }));
        let delivery = deliveries.lock().unwrap().remove(&spec.id).unwrap();
        let original = delivery.completed.clone();
        let JobSpec::Generate { outputs: routes, .. } = work else { panic!() };
        let converted = generate::produced(routes, &delivery.completed).unwrap();
        assert_eq!(converted.assets.len(), if m == FLUX { 4 } else { 1 });
        // Conversion refusal leaves original paid outputs owned by the host.
        let mut bad = work.clone();
        let JobSpec::Generate { outputs, .. } = &mut bad else { panic!() };
        outputs[0].modality = Modality::Document;
        assert!(generate::produced(outputs, &delivery.completed).is_err());
        assert_eq!(delivery.completed, original);
        let done = run.job_done(session.kinds(), spec.id, converted.outputs);
        assert!(done.failed.is_empty());
        assert_eq!(done.finished.len(), 1);
        assert!(run.is_finished());
        let step = &done.finished[0];
        let record = json!({"canvas":C,"results":[{"node":step.node,"generation":step.generation}]});
        // A refused record must not consume or alter the original CompletionDelivery.
        let mut refused = record.clone();
        refused["results"][0]["node"] = json!(NodeId::from_bytes([255; 16]));
        assert!(session.dispatch("run.record", &refused).is_err());
        assert_eq!(delivery.completed, original);
        // Tiny complete batch preflight: all bytes are authored, <64KiB total, empty store.
        assert!(converted.assets.iter().map(|(_, b)| b.len()).sum::<usize>() < 64 * 1024);
        for (h, bytes) in &converted.assets {
            assert_eq!(session.import_asset(bytes).unwrap(), *h);
        }
        assert_eq!(session.dispatch("run.record", &record).unwrap()["recorded"], 1);
        let node = |s: &Session| s.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap().clone();
        assert_eq!(node(&session).history.len(), 1);
        let recorded = node(&session).history[0].outputs[0].clone();
        if m == FLUX {
            let V::List(items) = recorded else { panic!() };
            assert_eq!(items.len(), 4);
            assert!(items.iter().all(|v| matches!(v, V::Media(_))));
        } else {
            assert!(matches!(recorded, V::Media(_)));
        }
        session.dispatch("edit.undo", &json!({})).unwrap();
        assert!(node(&session).history.is_empty());
        assert_eq!(delivery.completed, original);
        assert_eq!(transport.calls.load(Ordering::SeqCst), if m == FLUX { 7 } else { 4 });
        let rows = b.log.lock().unwrap();
        if m == TRELLIS {
            assert_eq!(rows.entries().map(|e| (e.status, e.cost)).collect::<Vec<_>>(), [(Charge::Actual, usd(250_000))]);
        } else {
            assert_eq!(rows.entries().count(), 0, "unknown usage is not a free monetary receipt");
        }
    }
}
#[test]
fn quote_start_refusals_never_invoke_service_or_provider() {
    for m in [FLUX, TRELLIS] {
        for kind in 0..5 {
            let caps = if kind == 0 { Budgets { per_call: Some(usd(0)), ..Default::default() } } else { Default::default() };
            let (mut s, called, _, transport) = fixture(m, caps);
            let mut v = consent(&mut s, kind != 1, if kind == 2 { 0 } else { 250_000 });
            if kind == 3 {
                v["accept"]["token"] = json!("stale");
            }
            if kind == 4 {
                let revision = s.revision();
                s.dispatch("node.title", &json!({"canvas":C,"id":N,"title":"Changed after quote"})).unwrap();
                assert_ne!(s.revision(), revision);
            }
            if kind == 2 && m != TRELLIS {
                continue;
            }
            if kind == 1 && m == TRELLIS {
                continue;
            }
            assert!(s.dispatch("run.start", &v).is_err(), "{m} {kind}");
            assert!(called.lock().unwrap().is_empty());
            assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        }
    }
}
#[test]
fn actual_runner_unknown_caps_lost_submit_and_cancellation_preserve_ownership() {
    for scope in 0..3 {
        let mut caps = Budgets::default();
        match scope {
            0 => caps.per_call = Some(usd(0)),
            1 => caps.session = Some(usd(100)),
            _ => caps.project = Some(usd(100)),
        };
        let b = budget(caps);
        let t = Arc::new(Transport { model: FLUX, calls: AtomicUsize::new(0), script: Script::Lost });
        let r =
            ProviderRunner::new(Arc::new(Immediate::default()), RunLimits::default(), Arc::new(|_, _| panic!()), Arc::new(|_, _, _| {})).with_budget(b.clone());
        r.register(
            JobId(1),
            QueuedJob::new(
                RegisteredProvider::default_instance(Arc::new(Fal::new(Secret::new("fixture-key-not-real").unwrap(), t.clone()))).unwrap(),
                ProviderJob {
                    model: FLUX.into(),
                    params: Default::default(),
                    inputs: vec![JobInput::Text { role: "prompt".into(), text: "a circle".into() }],
                    idempotency_key: "f".into(),
                },
                Some(JobPrice::new(N, "fal.flux-schnell".into(), None)),
            ),
        );
        let out = block_on(
            r.run(scheduler::Start { job: JobId(1), lane: scheduler::Lane::new("fal"), attempt: 1, idempotency_key: "f".into() }, CancelToken::default()),
        );
        assert!(matches!(out, scheduler::Outcome::Failed { transient: false, .. }));
        assert_eq!(t.calls.load(Ordering::SeqCst), 0);
    }
    for mode in 0..3 {
        let t = Arc::new(Transport { model: TRELLIS, calls: AtomicUsize::new(0), script: if mode == 0 { Script::Lost } else { Script::Pending } });
        let b = budget(Default::default());
        let r =
            ProviderRunner::new(Arc::new(Immediate::default()), RunLimits::default(), Arc::new(|_, _| panic!()), Arc::new(|_, _, _| {})).with_budget(b.clone());
        r.register(
            JobId(1),
            QueuedJob::new(
                RegisteredProvider::default_instance(Arc::new(Fal::new(Secret::new("fixture-key-not-real").unwrap(), t.clone()))).unwrap(),
                ProviderJob {
                    model: TRELLIS.into(),
                    params: Default::default(),
                    inputs: vec![JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: png().into() }],
                    idempotency_key: "f".into(),
                },
                Some(JobPrice::new(N, "fal.trellis2".into(), Some(usd(250_000)))),
            ),
        );
        let cancel = CancelToken::default();
        if mode == 1 {
            cancel.cancel();
        }
        let mut task = r.run(scheduler::Start { job: JobId(1), lane: scheduler::Lane::new("fal"), attempt: 1, idempotency_key: "f".into() }, cancel.clone());
        if mode == 2 {
            let w = futures::task::noop_waker();
            assert!(task.as_mut().poll(&mut std::task::Context::from_waker(&w)).is_pending());
            cancel.cancel();
        }
        let outcome = block_on(task);
        if mode == 0 {
            assert!(matches!(outcome, scheduler::Outcome::Failed { transient: false, .. }));
        } else {
            assert_eq!(outcome, scheduler::Outcome::Cancelled);
        }
        assert_eq!(t.calls.load(Ordering::SeqCst), usize::from(mode != 1));
        assert_eq!(b.log.lock().unwrap().entries().map(|e| e.status).collect::<Vec<_>>(), if mode == 1 { vec![] } else { vec![Charge::Unresolved] });
    }
}

#[test]
fn actual_driver_resumes_accepted_poll_failure_without_a_second_paid_create() {
    let runtime = Arc::new(Immediate::default());
    let t = Arc::new(Transport { model: TRELLIS, calls: AtomicUsize::new(0), script: Script::Resume });
    let b = budget(Default::default());
    let deliveries = Arc::new(AtomicUsize::new(0));
    let seen = deliveries.clone();
    let r = Arc::new(
        ProviderRunner::new(
            runtime.clone(),
            RunLimits { max_poll_errors: 0, ..Default::default() },
            Arc::new(move |_, _| {
                seen.fetch_add(1, Ordering::SeqCst);
            }),
            Arc::new(|_, _, _| {}),
        )
        .with_budget(b.clone()),
    );
    r.register(
        JobId(1),
        QueuedJob::new(
            RegisteredProvider::default_instance(Arc::new(Fal::new(Secret::new("fixture-key-not-real").unwrap(), t.clone()))).unwrap(),
            ProviderJob {
                model: TRELLIS.into(),
                params: Default::default(),
                inputs: vec![JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: png().into() }],
                idempotency_key: "resume-original".into(),
            },
            Some(JobPrice::new(N, "fal.trellis2".into(), Some(usd(250_000)))),
        ),
    );
    let mut scheduler = Scheduler::new(Default::default());
    scheduler
        .add(vec![scheduler::JobSpec {
            id: JobId(1),
            deps: vec![],
            lane: scheduler::Lane::new("fal"),
            retry: RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 1 },
            idempotency_key: "resume-original".into(),
        }])
        .unwrap();
    let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
    drop(handle);
    let mut events = vec![];
    let end = block_on(driver.run(r, runtime.clone(), |e| events.push(e)));
    assert_eq!(end.state(JobId(1)), Some(&JobState::Done));
    assert_eq!(events.iter().filter(|e| matches!(e, Event::Retrying { .. })).count(), 1);
    assert_eq!(t.calls.load(Ordering::SeqCst), 5);
    assert_eq!(deliveries.load(Ordering::SeqCst), 1);
    assert_eq!(b.log.lock().unwrap().entries().map(|e| (e.cost, e.status)).collect::<Vec<_>>(), [(usd(250_000), Charge::Actual)]);
}

#[test]
fn local_trellis_refusal_must_not_consume_finite_allowance_before_any_http() {
    let estimate = nodecraft_catalog::Catalog::builtin().unwrap().get("fal.trellis2").unwrap().quote(&Default::default()).unwrap().unwrap();
    assert_eq!(estimate, usd(250_000));
    let b = budget(Budgets { session: Some(estimate), project: Some(estimate), ..Default::default() });
    let t = Arc::new(Transport { model: TRELLIS, calls: AtomicUsize::new(0), script: Script::Good });
    let completed = Arc::new(AtomicUsize::new(0));
    let seen = completed.clone();
    let runtime = Arc::new(Immediate::default());
    let runner = Arc::new(
        ProviderRunner::new(
            runtime.clone(),
            RunLimits::default(),
            Arc::new(move |_, _| {
                seen.fetch_add(1, Ordering::SeqCst);
            }),
            Arc::new(|_, _, _| {}),
        )
        .with_budget(b.clone()),
    );
    runner.register(
        JobId(1),
        QueuedJob::new(
            RegisteredProvider::default_instance(Arc::new(Fal::new(Secret::new("fixture-key-not-real").unwrap(), t.clone()))).unwrap(),
            ProviderJob { model: TRELLIS.into(), params: Default::default(), inputs: vec![], idempotency_key: "local-refusal".into() },
            Some(JobPrice::new(N, "fal.trellis2".into(), Some(estimate))),
        ),
    );
    let mut scheduler = Scheduler::new(Default::default());
    scheduler
        .add(vec![scheduler::JobSpec {
            id: JobId(1),
            deps: vec![],
            lane: scheduler::Lane::new("fal"),
            retry: RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 1 },
            idempotency_key: "local-refusal".into(),
        }])
        .unwrap();
    let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
    drop(handle);
    let mut events = vec![];
    let end = block_on(driver.run(runner.clone(), runtime.clone(), |e| events.push(e)));
    assert!(matches!(end.state(JobId(1)), Some(JobState::Failed { .. })));
    assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })));
    assert_eq!(t.calls.load(Ordering::SeqCst), 0);
    assert_eq!(completed.load(Ordering::SeqCst), 0);
    let rows = b.log.lock().unwrap().entries().map(|e| (e.status, e.cost)).collect::<Vec<_>>();
    println!("local Trellis allowance={estimate:?} HTTP={} completions={} rows={rows:?}", t.calls.load(Ordering::SeqCst), completed.load(Ordering::SeqCst));
    assert!(rows.is_empty(), "pure local refusal cannot consume the finite allowance");
    runner.register(
        JobId(2),
        QueuedJob::new(
            RegisteredProvider::default_instance(Arc::new(Fal::new(Secret::new("fixture-key-not-real").unwrap(), t.clone()))).unwrap(),
            trellis_job("valid-after-missing", png().into()),
            Some(trellis_price()),
        ),
    );
    let mut scheduler = Scheduler::new(Default::default());
    scheduler.add(vec![driver_job(JobId(2), "valid-after-missing")]).unwrap();
    let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
    drop(handle);
    let end = block_on(driver.run(runner, runtime, |_| {}));
    assert_eq!(end.state(JobId(2)), Some(&JobState::Done));
    assert_eq!(t.calls.load(Ordering::SeqCst), 4);
    assert_eq!(completed.load(Ordering::SeqCst), 1);
    assert_eq!(b.log.lock().unwrap().entries().map(|e| (e.status, e.cost)).collect::<Vec<_>>(), [(Charge::Actual, estimate)]);
}

fn trellis_job(key: &str, image: Arc<[u8]>) -> ProviderJob {
    ProviderJob {
        model: TRELLIS.into(),
        params: Default::default(),
        inputs: vec![JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: image }],
        idempotency_key: key.into(),
    }
}
fn trellis_price() -> JobPrice {
    JobPrice::new(N, "fal.trellis2".into(), nodecraft_catalog::Catalog::builtin().unwrap().get("fal.trellis2").unwrap().quote(&Default::default()).unwrap())
}
fn driver_job(id: JobId, key: &str) -> scheduler::JobSpec {
    scheduler::JobSpec {
        id,
        deps: vec![],
        lane: scheduler::Lane::new("fal"),
        retry: RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 1 },
        idempotency_key: key.into(),
    }
}
#[test]
fn local_request_bound_and_corrupt_image_refusals_release_same_ledger_for_real_queue_completion() {
    for kind in 0..2 {
        let b = budget(Budgets { session: Some(usd(250_000)), project: Some(usd(250_000)), ..Default::default() });
        let t = Arc::new(Transport { model: TRELLIS, calls: AtomicUsize::new(0), script: Script::Good });
        let deliveries = Arc::new(AtomicUsize::new(0));
        let seen = deliveries.clone();
        let runtime = Arc::new(Immediate::default());
        let runner = Arc::new(
            ProviderRunner::new(
                runtime.clone(),
                RunLimits::default(),
                Arc::new(move |_, _| {
                    seen.fetch_add(1, Ordering::SeqCst);
                }),
                Arc::new(|_, _, _| {}),
            )
            .with_budget(b.clone()),
        );
        let limits = if kind == 0 { nodecraft_providers::fal::Limits { request_bytes: 1, ..Default::default() } } else { Default::default() };
        let image: Arc<[u8]> = if kind == 0 { png().into() } else { Arc::from(b"corrupt compressed image".as_slice()) };
        runner.register(
            JobId(1),
            QueuedJob::new(
                RegisteredProvider::default_instance(Arc::new(Fal::with_limits(Secret::new("fixture-key-not-real").unwrap(), t.clone(), limits).unwrap()))
                    .unwrap(),
                trellis_job("local-invalid", image),
                Some(trellis_price()),
            ),
        );
        let mut scheduler = Scheduler::new(Default::default());
        scheduler.add(vec![driver_job(JobId(1), "local-invalid")]).unwrap();
        let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
        drop(handle);
        let mut events = vec![];
        let end = block_on(driver.run(runner.clone(), runtime.clone(), |e| events.push(e)));
        assert!(matches!(end.state(JobId(1)), Some(JobState::Failed { .. })));
        assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })));
        assert_eq!(t.calls.load(Ordering::SeqCst), 0);
        assert_eq!(deliveries.load(Ordering::SeqCst), 0);
        assert_eq!(b.log.lock().unwrap().entries().count(), 0);
        runner.register(
            JobId(2),
            QueuedJob::new(
                RegisteredProvider::default_instance(Arc::new(Fal::new(Secret::new("fixture-key-not-real").unwrap(), t.clone()))).unwrap(),
                trellis_job("valid-after-local", png().into()),
                Some(trellis_price()),
            ),
        );
        let mut scheduler = Scheduler::new(Default::default());
        scheduler.add(vec![driver_job(JobId(2), "valid-after-local")]).unwrap();
        let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
        drop(handle);
        let end = block_on(driver.run(runner, runtime, |_| {}));
        assert_eq!(end.state(JobId(2)), Some(&JobState::Done));
        assert_eq!(t.calls.load(Ordering::SeqCst), 4);
        assert_eq!(deliveries.load(Ordering::SeqCst), 1);
        assert_eq!(b.log.lock().unwrap().entries().map(|e| (e.status, e.cost)).collect::<Vec<_>>(), [(Charge::Actual, usd(250_000))]);
    }
}

struct UncertainCreate {
    mode: u8,
    calls: AtomicUsize,
    posts: AtomicUsize,
}
impl Http for UncertainCreate {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable operation")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if r.method == HttpMethod::Post {
            self.posts.fetch_add(1, Ordering::SeqCst);
        }
        if n > 0 {
            assert_eq!(self.mode, 4);
            assert_eq!(r.method, HttpMethod::Put);
            return Box::pin(async { Ok(HttpResponse { status: 404, headers: vec![], body: vec![] }) });
        }
        assert_eq!(r.method, HttpMethod::Post);
        if self.mode < 2 {
            let kind = if self.mode == 0 { ErrorKind::Network } else { ErrorKind::UnsupportedInput };
            return Box::pin(async move { Err(ProviderError::new(kind, "constructor-side fixture error")) });
        }
        let body = if self.mode == 4 {
            serde_json::to_vec(&json!({"request_id":"accepted1","status":"UNKNOWN","status_url":"https://queue.fal.run/fal-ai/trellis-2/requests/accepted1/status","response_url":"https://queue.fal.run/fal-ai/trellis-2/requests/accepted1","cancel_url":"https://queue.fal.run/fal-ai/trellis-2/requests/accepted1/cancel"})).unwrap()
        } else {
            b"{".to_vec()
        };
        let status = if self.mode == 2 { 400 } else { 200 };
        Box::pin(async move { Ok(HttpResponse { status, headers: vec![], body }) })
    }
}
#[test]
fn transport_invocation_errors_and_accepted_receipt_cleanup_never_grant_local_proof() {
    for mode in 0..5 {
        let t = Arc::new(UncertainCreate { mode, calls: AtomicUsize::new(0), posts: AtomicUsize::new(0) });
        let b = budget(Budgets { session: Some(usd(250_000)), project: Some(usd(250_000)), ..Default::default() });
        let runtime = Arc::new(Immediate::default());
        let runner = Arc::new(
            ProviderRunner::new(runtime.clone(), RunLimits::default(), Arc::new(|_, _| panic!("no completed output")), Arc::new(|_, _, _| {}))
                .with_budget(b.clone()),
        );
        runner.register(
            JobId(1),
            QueuedJob::new(
                RegisteredProvider::default_instance(Arc::new(Fal::new(Secret::new("fixture-key-not-real").unwrap(), t.clone()))).unwrap(),
                trellis_job("sent-uncertain", png().into()),
                Some(trellis_price()),
            ),
        );
        let mut scheduler = Scheduler::new(Default::default());
        scheduler.add(vec![driver_job(JobId(1), "sent-uncertain")]).unwrap();
        let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
        drop(handle);
        let mut events = vec![];
        let end = block_on(driver.run(runner.clone(), runtime, |e| events.push(e)));
        assert!(matches!(end.state(JobId(1)), Some(JobState::Failed { .. })));
        assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })));
        runner.settle(JobId(1));
        assert_eq!(t.posts.load(Ordering::SeqCst), 1);
        assert_eq!(t.calls.load(Ordering::SeqCst), if mode == 4 { 2 } else { 1 });
        assert_eq!(b.log.lock().unwrap().entries().map(|e| (e.status, e.cost)).collect::<Vec<_>>(), [(Charge::Unresolved, usd(250_000))]);
    }
}
struct QueuedPollPending {
    calls: AtomicUsize,
    posts: AtomicUsize,
    cancel_status: u16,
}
impl Http for QueuedPollPending {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable operation")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            assert_eq!(r.method, HttpMethod::Post);
            self.posts.fetch_add(1, Ordering::SeqCst);
            let body=serde_json::to_vec(&json!({"request_id":"queued1","status":"IN_QUEUE","status_url":"https://queue.fal.run/fal-ai/trellis-2/requests/queued1/status","response_url":"https://queue.fal.run/fal-ai/trellis-2/requests/queued1","cancel_url":"https://queue.fal.run/fal-ai/trellis-2/requests/queued1/cancel"})).unwrap();
            return Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) });
        }
        if n == 1 {
            assert_eq!(r.method, HttpMethod::Get);
            return Box::pin(futures::future::pending());
        }
        assert_eq!(n, 2);
        assert_eq!(r.method, HttpMethod::Put);
        let status = self.cancel_status;
        let disposition = match status {
            202 => "CANCELLATION_REQUESTED",
            400 => "ALREADY_COMPLETED",
            404 => "NOT_FOUND",
            _ => panic!(),
        };
        let body = serde_json::to_vec(&json!({"status":disposition})).unwrap();
        Box::pin(async move { Ok(HttpResponse { status, headers: vec![], body }) })
    }
}
// Keep the cancellation deadline pending in this exact control. An immediately-ready
// fake timer would race the ready PUT future and could skip the cancellation request.
struct CancellationRuntime;
impl Runtime for CancellationRuntime {
    fn spawn(&self, task: TaskFuture<()>) {
        Immediate::default().spawn(task);
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        match ms {
            2_000 => Box::pin(async {}),                    // Fal's admitted queued poll interval.
            15_000 => Box::pin(futures::future::pending()), // Shared cancellation deadline.
            _ => panic!("unexpected cancellation-control timer"),
        }
    }
    fn now_ms(&self) -> u64 {
        0
    }
}

#[test]
fn cancellation_after_valid_queued_receipt_keeps_uncertainty_and_never_recreates() {
    for cancel_status in [202, 400, 404] {
        let t = Arc::new(QueuedPollPending { calls: AtomicUsize::new(0), posts: AtomicUsize::new(0), cancel_status });
        let b = budget(Default::default());
        let submitted = Arc::new(AtomicUsize::new(0));
        let seen = submitted.clone();
        let runner = ProviderRunner::new(
            Arc::new(CancellationRuntime),
            RunLimits::default(),
            Arc::new(|_, _| panic!("no completion")),
            Arc::new(move |_, _, update| {
                if matches!(update, nodecraft_providers::RunUpdate::Submitted(_)) {
                    seen.fetch_add(1, Ordering::SeqCst);
                }
            }),
        )
        .with_budget(b.clone());
        runner.register(
            JobId(1),
            QueuedJob::new(
                RegisteredProvider::default_instance(Arc::new(Fal::new(Secret::new("fixture-key-not-real").unwrap(), t.clone()))).unwrap(),
                trellis_job("accepted-cancel", png().into()),
                Some(trellis_price()),
            ),
        );
        let cancel = CancelToken::default();
        let mut task = runner
            .run(scheduler::Start { job: JobId(1), lane: scheduler::Lane::new("fal"), attempt: 1, idempotency_key: "accepted-cancel".into() }, cancel.clone());
        let w = futures::task::noop_waker();
        assert!(task.as_mut().poll(&mut std::task::Context::from_waker(&w)).is_pending());
        assert_eq!(submitted.load(Ordering::SeqCst), 1);
        assert_eq!(t.calls.load(Ordering::SeqCst), 2);
        cancel.cancel();
        assert_eq!(block_on(task), scheduler::Outcome::Cancelled);
        runner.settle(JobId(1));
        assert_eq!(t.posts.load(Ordering::SeqCst), 1);
        assert_eq!(t.calls.load(Ordering::SeqCst), 3);
        assert_eq!(b.log.lock().unwrap().entries().map(|e| (e.status, e.cost)).collect::<Vec<_>>(), [(Charge::Unresolved, usd(250_000))]);
    }
}

#[test]
fn factory_built_fal_keeps_unknown_price_consent_and_job_count_admission() {
    use nodecraft_engine::{
        provider_setup,
        secrets::{Account, MemoryStore, SecretStore},
        settings::ProviderSettings,
    };
    let keys = MemoryStore::default();
    keys.set(&Account::new("fal").unwrap(), &Secret::new("fixture-key-not-real").unwrap()).unwrap();
    for capped in [false, true] {
        let caps = if capped { Budgets { per_call: Some(usd(1_000_000)), ..Default::default() } } else { Budgets::default() };
        let (mut session, captured, _, transport) = fixture(FLUX, caps);
        let settings = ProviderSettings::default();
        let env = provider_setup::HostEnv { keys: &keys, http: transport.clone(), settings: &settings, now_ms: Arc::new(|| 0) };
        let setup = provider_setup::build(&nodecraft_catalog::Catalog::builtin().unwrap(), |adapter| provider_setup::make(adapter, &env));
        assert_eq!(setup.readiness("fal"), Some(&provider_setup::Readiness::Ready));
        session.set_provider_routing(setup.routing([(N, "fal")]).unwrap()).unwrap();
        let quote = session.dispatch("run.quote", &request()).unwrap();
        assert_eq!(quote["complete"], false);
        if capped {
            let accepted = consent(&mut session, true, 250_000);
            assert!(session.dispatch("run.start", &accepted).is_err());
            assert!(captured.lock().unwrap().is_empty());
        } else {
            let rejected = consent(&mut session, false, 250_000);
            assert!(session.dispatch("run.start", &rejected).is_err());
            let mut accepted = consent(&mut session, true, 250_000);
            accepted["accept"].as_object_mut().unwrap().remove("unpriced_jobs");
            assert!(session.dispatch("run.start", &accepted).is_err());
            accepted["accept"]["unpriced_jobs"] = json!(1);
            session.dispatch("run.start", &accepted).unwrap();
            let prepared = captured.lock().unwrap().pop().unwrap();
            let (run, progress) = session
                .authorize_prepared_run(prepared, RunOptions { id: [8; 16], first_job: 1, limits: Default::default(), retry: RetryPolicy::default() })
                .unwrap();
            let spec = &progress.jobs[0];
            let queued = run.queued_job(spec.id, &spec.idempotency_key).unwrap();
            assert!(queued.is_authorized());
            assert_eq!(queued.charged_price().unwrap().estimate, None);
            assert_eq!(run.unpriced_jobs(), (1, 0));
            assert_eq!(queued.provider.target().adapter(), "fal");
        }
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0, "admission never contacts Fal");
    }
}
