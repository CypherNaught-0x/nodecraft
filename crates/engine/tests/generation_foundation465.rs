//! Real production kind through typed admission/Driver/Fal adapter/record. No live calls.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::{Catalog, Price, Pricing, Unit};
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generate,
    generation_catalog::GenerationCatalog,
    jobs::{CompletionDelivery, JobBudget, ProviderRunner},
    provider_routing::ProviderRouting,
    run_cmds::{BudgetState, PreparedRun, RunCatalog},
};
use nodecraft_exec::{
    cost::{Budgets, Spent, UsageLog},
    driver::{Runtime, TaskFuture},
    run::RunOptions,
    scheduler::{JobId, JobState, Scheduler},
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::generation::KIND;
use nodecraft_providers::{
    BoxFuture, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError,
    fal::{FLUX, Fal},
    secrets::Secret,
};
use nodecraft_types::{CanvasId, Check, Condition, Contract, Currency, Money, NodeId, ParamOwner, ParamRule, ProjectId, Unsigned, Value};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
const C: CanvasId = CanvasId::from_bytes([65; 16]);
const N: NodeId = NodeId::from_bytes([66; 16]);
const MODEL: &str = "fixture/model-one";
fn png(n: u8) -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[n; 16], &Default::default()).unwrap()
}
fn catalog(unknown: bool) -> Catalog {
    let mut model = Catalog::builtin().unwrap().get("fal.flux-schnell").unwrap().clone();
    model.id = MODEL.into();
    model.params.params.iter_mut().find(|p| p.name == "num_images").unwrap().default = Some(Value::Unsigned(Unsigned(2)));
    model.params.params.iter_mut().find(|p| p.name == "seed").unwrap().default = Some(Value::Unsigned(Unsigned(7)));
    let mut contract = Contract::from_schema(MODEL, ParamOwner::Model, model.params.clone());
    contract.rules.push(ParamRule {
        enabled_when: Some(Condition::Equals { param: "num_images".into(), value: Value::Unsigned(Unsigned(1)) }),
        ..ParamRule::implied("seed")
    });
    contract.checks.push(Check {
        id: "two_images".into(),
        when: None,
        require: Condition::Equals { param: "num_images".into(), value: Value::Unsigned(Unsigned(2)) },
        message: "Choose two images for this fixture".into(),
    });
    model.parameter_contract = Some(contract);
    model.pricing = if unknown {
        Pricing::ByUsage { currency: Currency::USD }
    } else {
        Pricing::PerUnit { unit: Unit::Image, price: Price(Money::new(Currency::USD, 10)), count: Some("num_images".into()), default_count: None }
    };
    Catalog::from_models([model]).unwrap()
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
#[derive(Default)]
struct Transport {
    calls: AtomicUsize,
    submitted: Mutex<Vec<serde_json::Value>>,
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable path")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let root = "https://queue.fal.run/fal-ai/flux/requests/id1";
        let body = match n {
            0 => {
                assert_eq!(r.method, HttpMethod::Post);
                assert_eq!(r.url, format!("https://queue.fal.run/{FLUX}"));
                let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
                assert_eq!(v["num_images"], 2);
                assert!(v.get("seed").is_none(), "disabled default must not be reinserted");
                assert_eq!(v["prompt"], "Two circles");
                self.submitted.lock().unwrap().push(v);
                serde_json::to_vec(&json!({"request_id":"id1","status":"IN_QUEUE","status_url":format!("{root}/status"),"response_url":root,"cancel_url":format!("{root}/cancel")})).unwrap()
            }
            1 => {
                assert_eq!(r.url, format!("{root}/status"));
                serde_json::to_vec(&json!({"status":"COMPLETED"})).unwrap()
            }
            2 => {
                assert_eq!(r.url, root);
                serde_json::to_vec(
                    &json!({"images":[{"url":"https://fal.media/image0","width":2,"height":2},{"url":"https://fal.media/image1","width":2,"height":2}]}),
                )
                .unwrap()
            }
            3 | 4 => {
                assert!(r.headers.is_empty());
                assert_eq!(r.url, format!("https://fal.media/image{}", n - 3));
                png(if n == 3 { 80 } else { 160 })
            }
            _ => panic!("unexpected duplicate provider request"),
        };
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
    }
}
struct Fixture {
    session: Session,
    captured: Arc<Mutex<Vec<PreparedRun>>>,
    http: Arc<Transport>,
    snapshot: GenerationCatalog,
}
fn fixture(unknown: bool) -> Fixture {
    fixture_inner(unknown, false)
}
fn fixture_inner(unknown: bool, invalid_crosscheck: bool) -> Fixture {
    fixture_catalog(catalog(unknown), invalid_crosscheck, false)
}
fn fixture_catalog(catalog: Catalog, invalid_crosscheck: bool, wired: bool) -> Fixture {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([67; 16]), "Production generation").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = kinds
        .get(KIND)
        .unwrap()
        .instantiate_instance(
            N,
            Default::default(),
            &[("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text("Two circles".into()))].into(),
        )
        .unwrap();
    if invalid_crosscheck {
        node.params.insert("num_images".into(), Value::Unsigned(Unsigned(1)));
    }
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    if wired {
        let source = NodeId::from_bytes([90; 16]);
        let text = kinds
            .get("source.text")
            .unwrap()
            .instantiate_instance(source, Default::default(), &[("text".into(), Value::Text("Two circles".into()))].into())
            .unwrap();
        Change::AddNode { canvas: C, node: text }.apply(&mut project).unwrap();
        Change::Connect {
            canvas: C,
            edge: nodecraft_graph::Edge {
                id: nodecraft_types::EdgeId::from_bytes([91; 16]),
                from: nodecraft_graph::Endpoint { node: source, port: "out".into() },
                to: nodecraft_graph::Endpoint { node: N, port: "prompt".into() },
                order: 0,
            },
        }
        .apply(&mut project)
        .unwrap();
    }
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    resources.insert(BudgetState { budgets: Budgets::default(), spent: Spent::default() });
    let captured: Arc<Mutex<Vec<PreparedRun>>> = Arc::default();
    let sink = captured.clone();
    let mut services = Services::default();
    services
        .register_run_host(move |p| {
            sink.lock().unwrap().push(p);
            Ok(json!({"accepted":true}))
        })
        .unwrap();
    let http = Arc::new(Transport::default());
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Fal::new(Secret::new("fixture-not-real").unwrap(), http.clone()))).unwrap();
    routing.select(N, "fal", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    Fixture { session, captured, http, snapshot }
}
fn request() -> serde_json::Value {
    json!({"canvas":C,"mode":"selected","nodes":[N]})
}
fn consent(s: &mut Session, partial: bool) -> serde_json::Value {
    let q = s.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], true);
    let mut p = request();
    p["accept"] = json!({"token":q["token"],"partial":partial,"unpriced_jobs":1,"amounts":{"USD":20}});
    p
}
#[test]
fn production_shared_submission_quote_driver_ordered_outputs_record_and_undo() {
    for unknown in [false, true] {
        let mut f = fixture(unknown);
        let q = f.session.dispatch("run.quote", &request()).unwrap();
        if unknown {
            assert_eq!(q["complete"], false);
        } else {
            assert_eq!(q["totals"]["USD"]["micros"], 20);
        }
        let start = consent(&mut f.session, unknown);
        f.session.dispatch("run.start", &start).unwrap();
        let prepared = f.captured.lock().unwrap().pop().unwrap();
        assert_eq!(prepared.quote().steps[0].models, [MODEL]);
        let (mut run, progress) = f
            .session
            .authorize_prepared_run(prepared, RunOptions { id: [71; 16], first_job: 1, limits: Default::default(), retry: Default::default() })
            .unwrap();
        assert_eq!(progress.jobs.len(), 1);
        let spec = &progress.jobs[0];
        let queued = run.queued_job(spec.id, &spec.idempotency_key).unwrap();
        assert_eq!(queued.job.model, FLUX);
        assert!(!queued.job.params.contains_key("seed"));
        assert_eq!(queued.price.as_ref().unwrap().estimate, if unknown { None } else { Some(Money::new(Currency::USD, 20)) });
        let deliveries: Arc<Mutex<BTreeMap<JobId, CompletionDelivery>>> = Arc::default();
        let out = deliveries.clone();
        let runtime = Arc::new(Immediate::default());
        let budget = JobBudget {
            log: Arc::new(Mutex::new(UsageLog::default())),
            budgets: Arc::new(Mutex::new(Budgets::default())),
            session: "generation-fixture".into(),
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
        let delivery = deliveries.lock().unwrap().remove(&spec.id).unwrap();
        let original = delivery.completed.clone();
        let nodecraft_nodes::JobSpec::Generate { outputs: routes, .. } = run.work(spec.id).unwrap() else { panic!() };
        let converted = generate::produced(routes, &delivery.completed).unwrap();
        assert_eq!(converted.assets.len(), 2);
        let done = run.job_done(f.session.kinds(), spec.id, converted.outputs);
        assert!(done.failed.is_empty());
        assert_eq!(done.finished.len(), 1);
        let record = json!({"canvas":C,"results":[{"node":N,"generation":done.finished[0].generation}]});
        for (hash, bytes) in converted.assets {
            assert_eq!(f.session.import_asset(&bytes).unwrap(), hash);
        }
        f.session.dispatch("run.record", &record).unwrap();
        let node = |s: &Session| s.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap().clone();
        let Value::List(values) = &node(&f.session).history[0].outputs[0] else { panic!() };
        assert_eq!(values.len(), 2);
        assert_ne!(values[0], values[1]);
        assert_eq!(f.http.calls.load(Ordering::SeqCst), 5);
        assert_eq!(budget.log.lock().unwrap().entries().count(), if unknown { 0 } else { 1 });
        f.session.dispatch("edit.undo", &json!({})).unwrap();
        assert!(node(&f.session).history.is_empty());
        f.session.dispatch("edit.redo", &json!({})).unwrap();
        assert_eq!(node(&f.session).history.len(), 1);
        assert_eq!(delivery.completed, original);
        assert_eq!(f.http.calls.load(Ordering::SeqCst), 5);
    }
}
#[test]
fn production_catalog_replacement_without_revision_refuses_even_same_schema_new_remote() {
    let mut f = fixture(false);
    let revision = f.session.revision();
    let mut model = catalog(false).get(MODEL).unwrap().clone();
    model.provider_model = "fal-ai/other".into();
    let next = GenerationCatalog::new(RunCatalog::new(Catalog::from_models([model]).unwrap()).unwrap()).unwrap();
    let mut resources = Resources::default();
    resources.insert(next.run_catalog());
    resources.insert(next.clone());
    assert!(
        GenerationCatalog::current(&resources)
            .unwrap()
            .validate_instance_binding(f.session.kinds().get(KIND).unwrap(), f.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap())
            .is_err()
    );
    assert_ne!(f.snapshot.stamp(), next.stamp());
    f.session.swap_resource(Some(next.run_catalog()));
    f.session.swap_resource(Some(next));
    assert_eq!(f.session.revision(), revision);
    assert!(f.session.dispatch("run.quote", &request()).is_err());
    assert_eq!(f.http.calls.load(Ordering::SeqCst), 0);
    assert!(f.captured.lock().unwrap().is_empty());
}
#[test]
fn production_unknown_price_requires_explicit_consent_without_free_fallback() {
    let mut f = fixture(true);
    let start = consent(&mut f.session, false);
    assert!(f.session.dispatch("run.start", &start).is_err());
    assert!(f.captured.lock().unwrap().is_empty());
    assert_eq!(f.http.calls.load(Ordering::SeqCst), 0);
    let mut zero = consent(&mut f.session, true);
    zero["accept"]["unpriced_jobs"] = json!(0);
    assert!(f.session.dispatch("run.start", &zero).is_err());
    assert!(f.captured.lock().unwrap().is_empty());
    assert_eq!(f.http.calls.load(Ordering::SeqCst), 0);
    let start = consent(&mut f.session, true);
    f.session.dispatch("run.start", &start).unwrap();
    let prepared = f.captured.lock().unwrap().pop().unwrap();
    let (run, progress) =
        f.session.authorize_prepared_run(prepared, RunOptions { id: [71; 16], first_job: 1, limits: Default::default(), retry: Default::default() }).unwrap();
    let queued = run.queued_job(progress.jobs[0].id, &progress.jobs[0].idempotency_key).unwrap();
    assert!(queued.price.is_some());
    assert_eq!(queued.price.unwrap().estimate, None);
}
#[test]
fn production_crosscheck_rejection_is_before_service_or_provider_work() {
    let mut f = fixture(false);
    let original = f.session.document().clone();
    assert!(f.session.dispatch("node.param_set", &json!({"canvas":C,"id":N,"name":"num_images","value":{"type":"unsigned","value":"1"}})).is_err());
    assert_eq!(f.session.document(), &original);
    assert_eq!(f.http.calls.load(Ordering::SeqCst), 0);
    // A persisted graph can predate this validation. Quote still refuses it independently.
    let mut f = fixture_inner(false, true);
    let q = f.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], false);
    assert!(f.captured.lock().unwrap().is_empty());
    assert_eq!(f.http.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn production_shared_wiring_and_candidate_preserve_existing_node_owners() {
    let f = fixture(false);
    let canvas = f.session.document().canvases().get(&C).unwrap();
    let mut existing = canvas.nodes().get(&N).unwrap().clone();
    existing.title = "Preserved title".into();
    existing.pos.x = 77.0;
    let candidate = f.snapshot.prepare_instance(&existing, MODEL, &Default::default()).unwrap();
    assert_eq!(candidate.title, existing.title);
    assert_eq!(candidate.pos, existing.pos);
    assert_eq!(candidate.history, existing.history);
    let wiring = f.snapshot.capture_wiring(canvas, N).unwrap();
    f.snapshot.validate_destination_wiring(KIND, &wiring, MODEL, false).unwrap();
    f.snapshot.validate_wiring(canvas, &candidate, false).unwrap();
    assert_eq!(f.session.generation_catalog_stamp().unwrap(), f.snapshot.stamp());
    let mut a = blake3::Hasher::new();
    f.snapshot.stamp().bind(&mut a);
    let next = GenerationCatalog::new(RunCatalog::new(catalog(false)).unwrap()).unwrap();
    let mut b = blake3::Hasher::new();
    next.stamp().bind(&mut b);
    assert_ne!(a.finalize(), b.finalize());
    assert!(f.snapshot.validate_destination_wiring(KIND, &wiring, "unknown-model", false).is_err());
}

#[test]
fn producer_reset_never_stores_hidden_defaults_or_overrides_and_keep_retains_explicit() {
    let mut model = catalog(false).get(MODEL).unwrap().clone();
    model.parameter_contract.as_mut().unwrap().rules[0].hidden = nodecraft_types::HiddenPolicy::Reset;
    let snapshot = GenerationCatalog::new(RunCatalog::new(Catalog::from_models([model]).unwrap()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let kind = kinds.get(KIND).unwrap();
    let values: BTreeMap<_, _> = [("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text("Reset test".into()))].into();
    let node = kind.instantiate_instance(N, Default::default(), &values).unwrap();
    assert!(!node.params.contains_key("seed"));
    let mut explicit = values;
    explicit.insert("seed".into(), Value::Unsigned(Unsigned(99)));
    let node = kind.instantiate_instance(N, Default::default(), &explicit).unwrap();
    assert!(!node.params.contains_key("seed"));
    let candidate = snapshot.prepare_instance(&node, MODEL, &[("seed".into(), Value::Unsigned(Unsigned(99)))].into()).unwrap();
    assert!(!candidate.params.contains_key("seed"));
    let keep = fixture(false);
    let kept = keep.session.kinds().get(KIND).unwrap().instantiate_instance(N, Default::default(), &explicit).unwrap();
    assert_eq!(kept.params.get("seed"), Some(&Value::Unsigned(Unsigned(99))));
}
#[test]
fn builtin_real_image_models_share_only_verified_image_count_meaning() {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let a = snapshot.model(KIND, "openai-images.flare").unwrap();
    let b = snapshot.model(KIND, "openai-images.sunburst").unwrap();
    assert_eq!(Contract::shared_params(&[&a.parameters, &b.parameters]), ["n"]);
    let n = a.parameters.rule("n").unwrap();
    assert_eq!(n.unit, nodecraft_types::Unit::Count);
    assert_eq!(n.meaning.as_deref(), Some("generated_image_count"));
    let f = fixture(false);
    assert!(f.session.generation_catalog().is_ok());
}
#[test]
fn raw_production_model_selector_change_is_refused_without_mutating_or_provider_work() {
    let mut f = fixture(false);
    let before = f.session.document().clone();
    let revision = f.session.revision();
    let err = f.session.dispatch("node.param_set", &json!({"canvas":C,"id":N,"name":"model","value":{"type":"text","value":"other-model"}})).unwrap_err();
    assert_eq!(err.kind, nodecraft_types::ErrorKind::Unsupported);
    assert_eq!(f.session.document(), &before);
    assert_eq!(f.session.revision(), revision);
    assert_eq!(f.http.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn default_native_save_open_preserves_known_and_unknown_model_data_and_custom_registry() {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([90; 16]), "Generation native").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let known = kinds
        .get(KIND)
        .unwrap()
        .instantiate_instance(
            N,
            Default::default(),
            &[
                ("model".into(), Value::Text("openai-images.flare".into())),
                ("prompt".into(), Value::Text("A circle".into())),
                ("n".into(), Value::Unsigned(Unsigned(2))),
            ]
            .into(),
        )
        .unwrap();
    let mut unknown = known.clone();
    unknown.id = NodeId::from_bytes([91; 16]);
    unknown.params.insert("model".into(), Value::Text("unavailable/model".into()));
    unknown.params.insert("future_setting".into(), Value::Text("preserve me".into()));
    Change::AddNode { canvas: C, node: known.clone() }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: unknown.clone() }.apply(&mut project).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("generation.ncraft");
    let mut session = Session::with_storage(
        nodecraft_format::Snapshot { project: project.clone(), assets: Default::default(), epoch: 0 },
        Box::new(nodecraft_format::NativeStorage::new(dir.path().join("r1")).unwrap()),
    )
    .unwrap();
    session.dispatch("file.save_as", &json!({"location":path.to_str().unwrap()})).unwrap();
    drop(session);
    let empty = Project::new(ProjectId::from_bytes([92; 16]), "Empty").unwrap();
    let mut reopened = Session::with_storage(
        nodecraft_format::Snapshot { project: empty.clone(), assets: Default::default(), epoch: 0 },
        Box::new(nodecraft_format::NativeStorage::new(dir.path().join("r2")).unwrap()),
    )
    .unwrap();
    reopened.dispatch("file.open", &json!({"location":path.to_str().unwrap(),"discard_dirty":true})).unwrap();
    assert_eq!(reopened.document(), &project);
    assert!(reopened.kinds().get(KIND).is_ok());
    assert!(reopened.kinds().get(KIND).unwrap().validate_instance(&known, true).is_ok());
    assert!(reopened.kinds().get(KIND).unwrap().validate_instance(&unknown, true).is_err());
    assert_eq!(unknown.params.get("future_setting"), Some(&Value::Text("preserve me".into())));
    drop(reopened); // Release its native project lock before the independent background-open controls.
    // Background installation must retain the installed kind/resource authority, not
    // silently construct a fresh default catalog when the opened document arrives.
    struct HeldOpen(std::sync::Mutex<Option<nodecraft_engine::save_runner::SaveJob>>);
    impl nodecraft_engine::save_runner::SaveRunner for HeldOpen {
        fn spawn(&self, job: nodecraft_engine::save_runner::SaveJob) -> Result<(), nodecraft_engine::save_runner::SaveJob> {
            assert!(self.0.lock().unwrap().replace(job).is_none());
            Ok(())
        }
    }
    for stale in [false, true] {
        let mut opened = Session::with_sendable_storage(
            nodecraft_format::Snapshot { project: empty.clone(), assets: Default::default(), epoch: 0 },
            Box::new(nodecraft_format::NativeStorage::new(dir.path().join(if stale { "r4" } else { "r3" })).unwrap()),
        )
        .unwrap();
        let installed = opened.kinds().get(KIND).unwrap().definition_snapshot();
        assert!(installed.is_some());
        if stale {
            let replacement = GenerationCatalog::builtin().unwrap();
            opened.swap_resource(Some(replacement.run_catalog()));
            opened.swap_resource(Some(replacement));
        }
        let gate = Arc::new(HeldOpen(std::sync::Mutex::new(None)));
        opened.set_save_runner(gate.clone());
        assert_eq!(opened.dispatch("file.open", &json!({"location":path.to_str().unwrap(),"discard_dirty":true,"background":true})).unwrap()["opening"], true);
        assert_eq!(opened.document(), &empty);
        assert!(opened.poll_file().is_none());
        let refused = dir.path().join(if stale { "busy-stale.ncraft" } else { "busy-current.ncraft" });
        assert_eq!(opened.dispatch("file.save_as", &json!({"location":refused.to_str().unwrap()})).unwrap_err().kind, nodecraft_types::ErrorKind::Busy);
        assert!(!refused.exists(), "a refused save must not create its destination");
        gate.0.lock().unwrap().take().unwrap().run();
        assert_eq!(opened.document(), &empty, "I/O completion does not itself publish into Session");
        assert_eq!(opened.poll_file().unwrap().unwrap()["opened"], true);
        assert!(opened.poll_file().is_none());
        assert_eq!(opened.document(), &project);
        assert!(!opened.can_undo() && !opened.can_redo());
        assert_eq!(opened.kinds().get(KIND).unwrap().definition_snapshot(), installed);
        let current = opened.generation_catalog().unwrap();
        assert_eq!(current.validate_instance_binding(opened.kinds().get(KIND).unwrap(), &known).is_err(), stale);
        assert!(opened.kinds().get(KIND).unwrap().validate_instance(&unknown, true).is_err());
        let retained = opened.document().canvases().get(&C).unwrap().nodes().get(&unknown.id).unwrap();
        assert_eq!(retained.params.get("future_setting"), Some(&Value::Text("preserve me".into())));
    }
    let custom = Session::with_registries(empty, Registry::builtins().unwrap(), nodecraft_nodes::Registry::default()).unwrap();
    assert!(custom.kinds().get(KIND).is_err());
}

#[test]
fn production_missing_route_key_and_finite_budget_refuse_before_provider_work() {
    let mut missing = fixture(false);
    missing.session.set_provider_routing(ProviderRouting::new()).unwrap();
    let q = missing.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], false);
    assert!(missing.captured.lock().unwrap().is_empty());
    assert_eq!(missing.http.calls.load(Ordering::SeqCst), 0);
    let setup =
        nodecraft_engine::provider_setup::build(&catalog(false), |_| Err(nodecraft_engine::provider_setup::Readiness::NeedsKey { account: "fal".into() }));
    assert!(setup.routing([(N, "fal")]).is_err());
    assert_eq!(missing.http.calls.load(Ordering::SeqCst), 0);
    let mut capped = fixture(false);
    capped
        .session
        .swap_resource(Some(BudgetState { budgets: Budgets { per_call: Some(Money::new(Currency::USD, 19)), ..Default::default() }, spent: Spent::default() }));
    let q = capped.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], false);
    assert!(capped.captured.lock().unwrap().is_empty());
    assert_eq!(capped.http.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn production_staleness_uses_enabled_effective_values_not_hidden_overrides() {
    let f = fixture(false);
    let before = nodecraft_exec::staleness::input_hash(f.session.document().canvases().get(&C).unwrap(), f.session.kinds(), N).unwrap();
    let mut project = f.session.document().clone();
    let mut node = project.canvases().get(&C).unwrap().nodes().get(&N).unwrap().clone();
    node.params.insert("seed".into(), Value::Unsigned(Unsigned(99)));
    Change::SetNode { canvas: C, node: node.clone() }.apply(&mut project).unwrap();
    assert_eq!(nodecraft_exec::staleness::input_hash(project.canvases().get(&C).unwrap(), f.session.kinds(), N).unwrap(), before);
    node.params.insert("prompt".into(), Value::Text("Three circles".into()));
    Change::SetNode { canvas: C, node }.apply(&mut project).unwrap();
    assert_ne!(nodecraft_exec::staleness::input_hash(project.canvases().get(&C).unwrap(), f.session.kinds(), N).unwrap(), before);
}
#[test]
fn production_connected_prompt_order_and_media_refusal_are_pure_before_submit() {
    let f = fixture(false);
    let node = f.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap();
    let kind = f.session.kinds().get(KIND).unwrap();
    let mut targets = nodecraft_types::TargetSelection::empty();
    targets.insert(N, &nodecraft_types::ProviderTarget::default_instance("fal").unwrap()).unwrap();
    let inputs = vec![nodecraft_nodes::InputValue {
        port: "prompt".into(),
        port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Text, many: true },
        value: Value::List(vec![Value::Text("First".into()), Value::Text("Second".into())]),
    }];
    let jobs = kind.plan(&nodecraft_nodes::PlanCtx { node, inputs: &inputs, targets: &targets }).unwrap();
    let nodecraft_nodes::JobSpec::Generate { inputs: sent, .. } = &jobs[0] else { panic!() };
    assert_eq!(sent[0].value, Value::Text("First\nSecond".into()));
    let media = vec![nodecraft_nodes::InputValue {
        port: "image".into(),
        port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Image, many: true },
        value: Value::Media(nodecraft_types::AssetRef {
            hash: nodecraft_types::Blake3::from_bytes([0; 32]),
            mime: "image/png".parse().unwrap(),
            meta: Default::default(),
        }),
    }];
    // This model declares no image role (#502), so the wire reaches a port the card doesn't have.
    let refused = kind.plan(&nodecraft_nodes::PlanCtx { node, inputs: &media, targets: &targets }).unwrap_err();
    assert_eq!(refused.kind, nodecraft_types::ErrorKind::InvalidInput);
    assert!(refused.message.contains("unavailable port"), "{refused}");
    assert_eq!(f.http.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn production_required_non_inline_prompt_refuses_unwired_quote_but_accepts_actual_wire() {
    let mut model = catalog(false).get(MODEL).unwrap().clone();
    for mode in &mut model.modes {
        for role in &mut mode.inputs {
            if role.role == "prompt" {
                role.inline = false;
                role.min = 1;
            }
        }
    }
    let catalog = Catalog::from_models([model]).unwrap();
    let mut unwired = fixture_catalog(catalog.clone(), false, false);
    let before = unwired.session.document().clone();
    let q = unwired.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], false);
    assert!(unwired.captured.lock().unwrap().is_empty());
    assert_eq!(unwired.http.calls.load(Ordering::SeqCst), 0);
    assert_eq!(unwired.session.document(), &before);
    let mut wired = fixture_catalog(catalog, false, true);
    let q = wired.session.dispatch("run.quote", &request()).unwrap();
    assert_eq!(q["allowed"], true);
    assert_eq!(wired.http.calls.load(Ordering::SeqCst), 0);
}
