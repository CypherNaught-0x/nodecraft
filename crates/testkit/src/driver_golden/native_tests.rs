//! Native reactor companion. Traffic is bounded and loopback-only; no virtual clock.
#![cfg(not(target_arch = "wasm32"))]
// Test-only fixture helpers run under the same test panic allowances as assertions.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use nodecraft_engine::{
    Session,
    commands::Registry,
    jobs::{CompletionDelivery, JobBudget, ProviderRunner},
    provider_routing::{ProviderRouting, RegisteredProvider},
    run_cmds::{BudgetState, PreparedRun, RunCatalog},
};
use nodecraft_exec::{
    cost::{Budgets, Charge, UsageLog},
    driver::{Runtime, TaskFuture, driver},
    run::RunOptions,
    scheduler::{Event, JobId, JobState, Limits, RetryPolicy, Scheduler},
};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::{GenerateInput, GenerateOutput, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
use nodecraft_providers::{
    Provider, ReqwestHttp, RunLimits,
    llm::{ApiKey, Dialect, Endpoint, LlmClient},
};
use nodecraft_types::{CanvasId, Currency, Modality, Money, NodeId, PortType, ProjectId, Value};
use serde_json::json;
use std::{
    net::TcpListener,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);
struct TokioRuntime(Instant);
impl Runtime for TokioRuntime {
    fn spawn(&self, f: TaskFuture<()>) {
        tokio::spawn(f);
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(ms)))
    }
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}
struct Gen(NodeDescriptor);
impl NodeKind for Gen {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("generate").ok()
    }
    fn plan(&self, c: &PlanCtx<'_>) -> Result<Vec<JobSpec>, nodecraft_types::Error> {
        Ok(vec![JobSpec::Generate {
            node: c.node.id,
            provider: "native-build".into(),
            instance: c.targets.get(c.node.id, "native-build").and_then(|t| t.instance()),
            model: "model".into(),
            params: Default::default(),
            inputs: vec![GenerateInput { role: "user".into(), value: Value::Text("public fixture".into()) }],
            outputs: vec![GenerateOutput { port: "out".into(), modality: Modality::Text }],
        }])
    }
}
fn client(url: &str, bad: bool) -> Arc<super::Gated> {
    let inner = Arc::new(LlmClient::new(
        Endpoint {
            id: "native-build".into(),
            dialect: Dialect::OpenAi,
            base_url: url.into(),
            key: Some(ApiKey::new(if bad { "public-invalid\nfixture" } else { "public-fixture" })),
        },
        Arc::new(ReqwestHttp::new().unwrap()),
    ));
    let submit_gate = super::Gate::default();
    submit_gate.open();
    let poll_gate = super::Gate::default();
    poll_gate.open();
    Arc::new(super::Gated {
        mock: nodecraft_providers::mock::MockProvider::new("native-build", []),
        submit_gate,
        poll_gate,
        calls: Mutex::new(super::Calls::default()),
        global: Arc::default(),
        fault: super::Fault::default(),
        synthetic: false,
        inner,
    })
}
fn prepared(provider: Arc<dyn Provider>) -> (Session, nodecraft_engine::run_cmds::AuthorizedRun, nodecraft_exec::run::Progress) {
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut d = kinds.get("source.text").unwrap().descriptor().clone();
    d.id = "driver.native".into();
    d.category = nodecraft_nodes::Category::Generation;
    d.quick_add = None;
    d.inputs.clear();
    d.params.params.clear();
    d.params.params.push(nodecraft_types::ParamSpec {
        name: "model".into(),
        label: "Model".into(),
        description: None,
        control: nodecraft_types::ParamControl::Model,
        required: true,
        default: Some(Value::Text("native-model".into())),
        visible_if: None,
    });
    kinds.register(Box::new(Gen(d))).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([3; 16]), "Native Driver fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let node = kinds.get("driver.native").unwrap().instantiate_instance(N, Default::default(), &Default::default()).unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap();
    let catalog = nodecraft_catalog::Catalog::from_toml(
        r#"[[model]]
id="native-model"
provider="native-build"
provider_model="model"
label="Native fixture"
description="Public loopback"
quality=1
latency_s=0
pricing={type="per_call",price="0.10 USD"}
[[model.modes]]
id="generate"
output="text"
"#,
    )
    .unwrap();
    session.swap_resource(Some(RunCatalog::new(catalog).unwrap()));
    session.swap_resource(Some(BudgetState {
        budgets: Budgets { project: Some(Money::new(Currency::USD, 1_000_000)), ..Default::default() },
        spent: Default::default(),
    }));
    let mut routing = ProviderRouting::default();
    let binding = routing.bind_default(provider).unwrap();
    routing.select(N, "native-build", binding.target()).unwrap();
    session.set_provider_routing(routing).unwrap();
    let queue = Arc::new(Mutex::new(None::<PreparedRun>));
    let q = queue.clone();
    session
        .register_run_host(move |p| {
            *q.lock().unwrap() = Some(p);
            Ok(json!({"queued":true}))
        })
        .unwrap();
    let mut params = json!({"canvas":C,"mode":"selected","nodes":[N]});
    let quote = session.dispatch("run.quote", &params).unwrap();
    params["accept"] = json!({"token":quote["token"],"amounts":{"USD":100_000}});
    session.dispatch("run.start", &params).unwrap();
    let p = queue.lock().unwrap().take().unwrap();
    let (run, progress) = session
        .authorize_prepared_run(
            p,
            RunOptions { id: [0x71; 16], first_job: 1, retry: RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 }, limits: Default::default() },
        )
        .unwrap();
    (session, run, progress)
}
fn listener() -> (TcpListener, String) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    (l, url)
}
async fn serve(l: TcpListener, reply: bool) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(30), async move {
        let l = tokio::net::TcpListener::from_std(l).unwrap();
        let (stream, _) = l.accept().await.unwrap();
        let mut bytes = Vec::with_capacity(65536);
        loop {
            stream.readable().await.unwrap();
            let mut buf = [0; 4096];
            match stream.try_read(&mut buf) {
                Ok(0) => panic!("fixture client closed before complete request"),
                Ok(n) => {
                    assert!(bytes.len() + n <= 65536);
                    bytes.extend_from_slice(&buf[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(e) => panic!("fixture read {e}"),
            }
            if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                let header = std::str::from_utf8(&bytes[..end]).unwrap();
                let len = header
                    .lines()
                    .find_map(|s| s.to_ascii_lowercase().strip_prefix("content-length:").map(|n| n.trim().parse::<usize>().unwrap()))
                    .unwrap_or(0);
                assert!(len <= 60000);
                if bytes.len() >= end + 4 + len {
                    break;
                }
            }
        }
        if reply {
            let body = br#"{"choices":[{"message":{"role":"assistant","content":"public ok"}}]}"#;
            let mut response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
            response.extend_from_slice(body);
            let mut n = 0;
            while n < response.len() {
                stream.writable().await.unwrap();
                match stream.try_write(&response[n..]) {
                    Ok(0) => panic!("fixture write stopped"),
                    Ok(w) => n += w,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("fixture write {e}"),
                }
            }
        }
        bytes
    })
    .await
    .expect("bounded loopback")
}
struct Driven {
    session: Session,
    events: Vec<Event>,
    scheduler: Scheduler,
    budget: JobBudget,
    deliveries: Arc<Mutex<Vec<CompletionDelivery>>>,
}
async fn drive(url: &str, bad: bool, replace: bool) -> Driven {
    let (session, run, progress) = prepared(client(url, bad));
    assert_eq!(progress.jobs.len(), 1);
    let runtime = Arc::new(TokioRuntime(Instant::now()));
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets { project: Some(Money::new(Currency::USD, 1_000_000)), ..Default::default() })),
        session: "native-fixture".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let deliveries = Arc::new(Mutex::new(Vec::with_capacity(3)));
    let sink = deliveries.clone();
    let runner = Arc::new(
        ProviderRunner::new(
            runtime.clone(),
            RunLimits { deadline_ms: 20_000, ..Default::default() },
            Arc::new(move |_, d| {
                let mut s = sink.lock().unwrap();
                assert!(s.len() < 3);
                s.push(d);
            }),
            Arc::new(|_, _, _| {}),
        )
        .with_budget(budget.clone()),
    );
    let spec = &progress.jobs[0];
    let queued = run.queued_job(spec.id, &spec.idempotency_key).unwrap();
    runner.register(spec.id, queued.clone());
    let mut scheduler = Scheduler::new(Limits::default());
    scheduler.add(progress.jobs).unwrap();
    let (driver, handle) = driver(scheduler);
    drop(handle);
    let mut events = vec![];
    let replacement = client(url, true);
    let r = runner.clone();
    let scheduler = tokio::time::timeout(
        Duration::from_secs(30),
        driver.run(r, runtime, |event| {
            if replace && matches!(event, Event::Retrying { .. }) {
                let mut next = queued.clone();
                next.provider = RegisteredProvider::default_instance(replacement.clone()).unwrap();
                runner.register(JobId(1), next);
            }
            events.push(event);
        }),
    )
    .await
    .expect("bounded native Driver");
    runner.settle(JobId(1));
    runner.forget(JobId(1));
    Driven { session, events, scheduler, budget, deliveries }
}
#[tokio::test(flavor = "current_thread")]
async fn fresh_build_not_sent_has_one_attempt_and_actual_driver_positive() {
    let (l, url) = listener();
    let failed = drive(&url, true, false).await;
    assert!(matches!(l.accept(),Err(e)if e.kind()==std::io::ErrorKind::WouldBlock));
    assert!(matches!(failed.scheduler.state(JobId(1)), Some(JobState::Failed { .. })));
    assert_eq!(failed.events.iter().filter(|e| matches!(e, Event::Started { .. })).count(), 1);
    assert_eq!(failed.budget.log.lock().unwrap().entries().count(), 0);
    assert_eq!(failed.budget.log.lock().unwrap().metadata_statistics().held, 0);
    assert!(failed.deliveries.lock().unwrap().is_empty());
    check_native("fresh", &failed);
    let (done, wire) = tokio::join!(drive(&url, false, false), serve(l, true));
    assert!(wire.starts_with(b"POST "));
    assert!(matches!(done.scheduler.state(JobId(1)), Some(JobState::Done)));
    assert_eq!(done.deliveries.lock().unwrap().len(), 1);
    assert_eq!(done.budget.log.lock().unwrap().entries().count(), 1);
    assert!(!done.session.can_undo());
    check_native("positive", &done);
}
#[tokio::test(flavor = "current_thread")]
async fn earlier_ambiguous_request_keeps_charge_after_actual_builder_failure() {
    let (l, url) = listener();
    let (done, wire) = tokio::join!(drive(&url, false, true), serve(l, false));
    assert!(wire.starts_with(b"POST "));
    assert_eq!(done.events.iter().filter(|e| matches!(e, Event::Started { .. })).count(), 2);
    assert_eq!(done.events.iter().filter(|e| matches!(e, Event::Retrying { .. })).count(), 1);
    assert!(matches!(done.scheduler.state(JobId(1)), Some(JobState::Failed { .. })));
    assert_eq!(done.budget.log.lock().unwrap().entries().map(|e| e.status).collect::<Vec<_>>(), vec![Charge::Unresolved]);
    assert!(done.deliveries.lock().unwrap().is_empty());
    check_native("ambiguous", &done);
}

fn check_native(name: &str, d: &Driven) {
    let log = d.budget.log.lock().unwrap();
    let charges: Vec<_> = log.entries().map(|e| json!({"micros":e.cost.micros,"status":e.status})).collect();
    let report = json!({"attempts":d.events.iter().filter(|e|matches!(e,Event::Started{..})).count(),"retrying":d.events.iter().filter(|e|matches!(e,Event::Retrying{..})).count(),"terminal":if matches!(d.scheduler.state(JobId(1)),Some(JobState::Done)){"done"}else{"failed"},"charges":charges,"held":log.metadata_statistics().held,"deliveries":d.deliveries.lock().unwrap().len()});
    let expected: serde_json::Value = serde_json::from_str(include_str!("../../tests/golden_driver/native.report.json")).unwrap();
    assert_eq!(report, expected[name]);
}
