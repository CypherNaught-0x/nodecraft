//! Real Replicate adapter -> no-redirect Http contract -> ProviderRunner -> scheduler Driver.
//! Synthetic local transport acceptance, not evidence of Replicate processing or a paid request.
#![cfg(not(target_arch = "wasm32"))]
use futures::executor::block_on;
use nodecraft_engine::jobs::{JobBudget, JobPrice, ProviderRunner, QueuedJob};
use nodecraft_exec::cost::{Budgets, Charge, UsageLog};
use nodecraft_exec::driver::{CancelToken, JobRunner, Runtime, TaskFuture};
use nodecraft_exec::scheduler::{Event, JobId, JobSpec, JobState, Lane, Limits, Outcome, RetryPolicy, Scheduler, Start};
use nodecraft_providers::replicate::{HELLO, Replicate};
use nodecraft_providers::secrets::Secret;
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
    Prediction { lose_poll: bool },
    DownloadAuth,
}
struct Transport {
    script: Script,
    requests: AtomicUsize,
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("Replicate must opt in to no redirect")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        assert!(r.url.contains("/predictions") || (matches!(&self.script, Script::DownloadAuth) && r.url.starts_with("https://replicate.delivery/")));
        if r.url.ends_with("/predictions") {
            assert!(!r.body.is_empty());
        }
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
            Script::Prediction { lose_poll } => {
                let n = self.requests.load(Ordering::SeqCst);
                assert_eq!(r.method, if n == 1 { nodecraft_providers::HttpMethod::Post } else { nodecraft_providers::HttpMethod::Get });
                if *lose_poll && n == 2 {
                    return Box::pin(async { Err(ProviderError::new(ErrorKind::Network, "lost poll")) });
                }
                let (model, version) = HELLO.split_once(':').unwrap();
                let body = serde_json::to_vec(&serde_json::json!({"id":"fixture-prediction","model":model,"version":version,"status":if n==1 {"starting"} else {"succeeded"},"output":"Hello fixture"})).unwrap();
                Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
            }
            Script::DownloadAuth => {
                let n = self.requests.load(Ordering::SeqCst);
                assert_eq!(r.method, if n == 1 { nodecraft_providers::HttpMethod::Post } else { nodecraft_providers::HttpMethod::Get });
                let (model, version) = nodecraft_providers::replicate::SDXL.split_once(':').unwrap();
                let body = serde_json::to_vec(&serde_json::json!({"id":"fixture-prediction","model":model,"version":version,"status":if n==1 {"starting"}else{"succeeded"},"output":["https://replicate.delivery/expired"]})).unwrap();
                Box::pin(async move { Ok(HttpResponse { status: if n == 3 { 403 } else { 200 }, headers: vec![], body }) })
            }
            Script::Pending => Box::pin(futures::future::pending()),
        }
    }
}
fn money(n: u64) -> Money {
    Money::new(Currency::USD, n)
}
fn registration(http: Arc<Transport>, price: Option<Option<Money>>) -> QueuedJob {
    QueuedJob {
        provider: nodecraft_engine::provider_routing::RegisteredProvider::default_instance(Arc::new(Replicate::new(
            Secret::new("fixture-key-not-real").unwrap(),
            http,
        )))
        .unwrap(),
        job: ProviderJob {
            model: HELLO.into(),
            params: serde_json::Map::new(),
            inputs: vec![JobInput::Text { role: "prompt".into(), text: "A blue circle".into() }],
            idempotency_key: "fixture-prediction-1".into(),
        },
        price: price.map(|estimate| JobPrice::new(NodeId::from_bytes([1; 16]), "replicate.hello".into(), estimate)),
    }
}
fn runner(budgets: Budgets) -> (Arc<ProviderRunner>, JobBudget, Arc<AtomicUsize>) {
    runner_limits(budgets, RunLimits::default())
}
fn runner_limits(budgets: Budgets, limits: RunLimits) -> (Arc<ProviderRunner>, JobBudget, Arc<AtomicUsize>) {
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
        limits,
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
            lane: Lane::new("replicate"),
            retry: RetryPolicy { max_attempts: 4, base_delay_ms: 1, max_delay_ms: 2 },
            idempotency_key: "fixture-prediction-1".into(),
        }])
        .unwrap();
    let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
    drop(handle);
    let mut events = vec![];
    let end = block_on(driver.run(runner, Arc::new(Immediate(AtomicU64::new(0))), |event| events.push(event)));
    (end, events)
}
#[test]
fn real_replicate_adapter_never_automatically_replays_an_uncertain_submit() {
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
#[test]
fn cancellation_before_and_during_real_replicate_transport_preserves_billing_uncertainty() {
    for pending in [false, true] {
        let t = Arc::new(Transport { script: Script::Pending, requests: AtomicUsize::new(0) });
        let (r, b, _) = runner(Budgets::default());
        r.register(JobId(1), registration(t.clone(), Some(Some(money(7)))));
        let cancel = CancelToken::default();
        if !pending {
            cancel.cancel();
        }
        let mut work = r.run(Start { job: JobId(1), lane: Lane::new("replicate"), attempt: 1, idempotency_key: "fixture-prediction-1".into() }, cancel.clone());
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
fn accepted_prediction_resumes_after_lost_poll_without_a_second_create() {
    for lose_poll in [false, true] {
        let t = Arc::new(Transport { script: Script::Prediction { lose_poll }, requests: AtomicUsize::new(0) });
        let (r, b, out) = runner(Budgets::default());
        r.register(JobId(1), registration(t.clone(), Some(None)));
        let (s, events) = drive(r);
        assert!(matches!(s.state(JobId(1)), Some(JobState::Done)));
        assert_eq!(out.load(Ordering::SeqCst), 1);
        assert_eq!(t.requests.load(Ordering::SeqCst), if lose_poll { 3 } else { 2 });
        assert_eq!(events.iter().filter(|e| matches!(e, Event::Retrying { .. })).count(), 0);
        assert_eq!(b.log.lock().unwrap().entries().count(), 0);
    }
}

#[test]
fn exhausted_internal_poll_retry_is_resumed_by_driver_with_the_same_prediction() {
    let t = Arc::new(Transport { script: Script::Prediction { lose_poll: true }, requests: AtomicUsize::new(0) });
    let (r, b, out) = runner_limits(Budgets::default(), RunLimits { max_poll_errors: 0, ..RunLimits::default() });
    r.register(JobId(1), registration(t.clone(), Some(Some(money(7)))));
    let (s, events) = drive(r);
    assert!(matches!(s.state(JobId(1)), Some(JobState::Done)));
    assert_eq!(out.load(Ordering::SeqCst), 1);
    // The transport asserts POST only at request 1, GET on both subsequent requests.
    assert_eq!(t.requests.load(Ordering::SeqCst), 3);
    assert_eq!(events.iter().filter(|e| matches!(e, Event::Retrying { .. })).count(), 1);
    let entries = b.log.lock().unwrap().entries().map(|e| (e.cost, e.status)).collect::<Vec<_>>();
    // Existing runner settlement uses its host-supplied estimate when provider cost is absent.
    assert_eq!(entries, vec![(money(7), Charge::Actual)]);
}

#[test]
fn rejected_create_cancels_once_and_preserves_safe_id_in_driver_failure_after_settlement() {
    for escaped in [false, true] {
        let (model, _) = HELLO.split_once(':').unwrap();
        let body =
            serde_json::to_vec(&serde_json::json!({"id":"retained-accepted-id","model":model,"version":"hidden","status":"processing","output":null})).unwrap();
        let body = if escaped { String::from_utf8(body).unwrap().replace("retained-accepted-id", r"\u0072etained-accepted-id").into_bytes() } else { body };
        let t = Arc::new(Transport { script: Script::Response(200, body), requests: AtomicUsize::new(0) });
        let (r, b, out) = runner(Budgets::default());
        r.register(JobId(1), registration(t.clone(), Some(Some(money(7)))));
        let (mut scheduler, events) = drive(r.clone());
        let Some(JobState::Failed { error }) = scheduler.state(JobId(1)) else { panic!("failed") };
        assert!(error.contains("retained-accepted-id") && error.contains("unconfirmed"));
        assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })));
        assert_eq!(out.load(Ordering::SeqCst), 0);
        assert_eq!(t.requests.load(Ordering::SeqCst), 2, "one create and one best-effort cancel");
        r.settle(JobId(1));
        assert_eq!(b.log.lock().unwrap().entries().map(|e| (e.cost, e.status)).collect::<Vec<_>>(), [(money(7), Charge::Unresolved)]);
        r.forget(JobId(1));
        let Some(JobState::Failed { error }) = scheduler.state(JobId(1)) else { panic!("diagnostic remains in scheduler") };
        assert!(error.contains("retained-accepted-id"));
        scheduler.forget_finished();
        assert!(scheduler.state(JobId(1)).is_none(), "explicitly forgetting the failure also loses its diagnostic; no durable handle promised");
        assert_eq!(t.requests.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn inaccessible_cdn_link_is_terminal_without_api_key_advice_or_another_create() {
    let t = Arc::new(Transport { script: Script::DownloadAuth, requests: AtomicUsize::new(0) });
    let (r, b, out) = runner(Budgets::default());
    let mut q = registration(t.clone(), Some(Some(money(7))));
    q.job.model = nodecraft_providers::replicate::SDXL.into();
    r.register(JobId(1), q);
    let (s, events) = drive(r);
    let Some(JobState::Failed { error }) = s.state(JobId(1)) else { panic!("failure") };
    assert!(error.contains("output link"));
    assert!(!error.contains("API key"));
    assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })));
    assert_eq!(t.requests.load(Ordering::SeqCst), 3);
    assert_eq!(out.load(Ordering::SeqCst), 0);
    assert_eq!(b.log.lock().unwrap().entries().map(|e| e.status).collect::<Vec<_>>(), [Charge::Unresolved]);
}
