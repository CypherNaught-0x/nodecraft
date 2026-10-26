//! Automatic replay policy is independent of diagnostic transience and billing evidence.
use super::*;
use nodecraft_exec::scheduler::{Event, JobSpec, JobState, Limits, RetryPolicy, Scheduler};
use std::sync::atomic::AtomicBool;

struct LostReply {
    calls: AtomicU64,
    error: ProviderError,
    ready: AtomicBool,
}
impl LostReply {
    fn new(kind: ErrorKind) -> Arc<Self> {
        Arc::new(Self { calls: AtomicU64::new(0), error: ProviderError::new(kind, "public accepted-but-lost fixture"), ready: AtomicBool::new(true) })
    }
}
impl Provider for LostReply {
    fn id(&self) -> &str {
        "policy-fixture"
    }
    fn submit<'a>(&'a self, _: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::future::poll_fn(|_| {
            if self.ready.load(Ordering::SeqCst) { std::task::Poll::Ready(Err(self.error.clone())) } else { std::task::Poll::Pending }
        }))
    }
    fn poll<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async { Err(ProviderError::new(ErrorKind::NotFound, "no fixture handle")) })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Forwards actual evidence as well as the legacy entry, without fabricating opaque proofs.
struct NoReplay {
    inner: Arc<dyn Provider>,
    invocations: AtomicU64,
}
impl NoReplay {
    fn new(inner: Arc<dyn Provider>) -> Arc<Self> {
        Arc::new(Self { inner, invocations: AtomicU64::new(0) })
    }
}
impl Provider for NoReplay {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.inner.submit(job)
    }
    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, c: &'a nodecraft_providers::SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        self.inner.submit_with_delivery(job, c)
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        self.inner.poll(handle)
    }
    fn cancel<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        self.inner.cancel(handle)
    }
}

/// These driver fixtures have immediately-ready jobs and virtual sleeps. Poll spawned work
/// directly once; nested block_on would be an invalid executor fixture, not a runner failure.
struct InlineRuntime(AtomicU64);
impl Runtime for InlineRuntime {
    fn spawn(&self, mut task: TaskFuture<()>) {
        let waker = futures::task::noop_waker();
        assert!(task.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_ready(), "driver fixture unexpectedly needs asynchronous progress");
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        self.0.fetch_add(ms, Ordering::SeqCst);
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn drive(runner: Arc<ProviderRunner>) -> (Scheduler, Vec<Event>) {
    let mut scheduler = Scheduler::new(Limits::default());
    scheduler
        .add(vec![JobSpec {
            id: JobId(1),
            deps: vec![],
            lane: Lane::new("fixture"),
            retry: RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 },
            idempotency_key: "nodecraft:r:1".into(),
        }])
        .unwrap();
    let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
    drop(handle); // Drain existing work, including legitimate backoff retries, then terminate.
    let mut events = vec![];
    let scheduler = block_on(driver.run(runner, Arc::new(InlineRuntime(AtomicU64::new(0))), |event| events.push(event)));
    (scheduler, events)
}
fn statuses(budget: &JobBudget) -> Vec<Charge> {
    budget.log.lock().unwrap().entries().map(|entry| entry.status).collect()
}

#[test]
fn ambiguous_terminal_policy_stops_actual_driver_retries_and_closes_charge() {
    for kind in [ErrorKind::Network, ErrorKind::Timeout, ErrorKind::Server, ErrorKind::Busy, ErrorKind::RateLimited, ErrorKind::Malformed] {
        let lost = LostReply::new(kind);
        let provider = NoReplay::new(lost.clone());
        let (runner, budget) = budgeted(Some(1_000_000));
        runner.register(JobId(1), priced(provider.clone(), Some(100_000)));
        let (scheduler, events) = drive(Arc::new(runner));
        assert_eq!(lost.calls.load(Ordering::SeqCst), 1, "{kind:?}");
        assert_eq!(provider.invocations.load(Ordering::SeqCst), 1);
        assert!(!events.iter().any(|event| matches!(event, Event::Retrying { .. })), "{events:?}");
        assert!(matches!(scheduler.state(JobId(1)), Some(JobState::Failed { error }) if error == &lost.error.to_string()), "{kind:?}");
        assert_eq!(spent(&budget), 100_000);
        assert_eq!(statuses(&budget), [Charge::Unresolved], "terminal despite transient kind, before host settle");
    }
}

#[test]
fn legacy_default_keeps_retrying_with_the_same_charge() {
    let provider = LostReply::new(ErrorKind::Network);
    assert!(provider.retry_ambiguous_submit());
    let (runner, budget) = budgeted(Some(1_000_000));
    runner.register(JobId(1), priced(provider.clone(), Some(100_000)));
    let runner = Arc::new(runner);
    let (_, events) = drive(runner.clone());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert_eq!(events.iter().filter(|event| matches!(event, Event::Retrying { .. })).count(), 2);
    assert_eq!(spent(&budget), 100_000);
    assert_eq!(statuses(&budget), [Charge::Estimate]);
    runner.settle(JobId(1));
    assert_eq!(statuses(&budget), [Charge::Unresolved]);
}

#[test]
fn azure_lost_reply_stops_actual_driver_retries_and_keeps_the_charge_unresolved() {
    use nodecraft_providers::llm::{ApiKey, Endpoint, LlmClient};
    struct LostHttp(AtomicU64);
    impl nodecraft_providers::Http for LostHttp {
        fn send(&self, _: nodecraft_providers::HttpRequest) -> BoxFuture<'_, Result<nodecraft_providers::HttpResponse, ProviderError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(ProviderError::new(ErrorKind::Network, "public Azure accepted-but-lost fixture")) })
        }
    }
    let http = Arc::new(LostHttp(AtomicU64::new(0)));
    let provider = Arc::new(LlmClient::new(Endpoint::azure_openai("https://demo.openai.azure.com", ApiKey::new("public-fixture")).unwrap(), http.clone()));
    assert!(!provider.retry_ambiguous_submit());
    let (runner, budget) = budgeted(Some(1_000_000));
    let mut queued = priced(provider, Some(100_000));
    queued.job.model = "public-deployment".into();
    queued.job.inputs = vec![nodecraft_providers::JobInput::Text { role: "prompt".into(), text: "A harbour".into() }];
    runner.register(JobId(1), queued);
    let (scheduler, events) = drive(Arc::new(runner));
    assert_eq!(http.0.load(Ordering::SeqCst), 1);
    assert!(!events.iter().any(|event| matches!(event, Event::Retrying { .. })), "{events:?}");
    assert!(matches!(scheduler.state(JobId(1)), Some(JobState::Failed { .. })));
    assert_eq!(spent(&budget), 100_000);
    assert_eq!(statuses(&budget), [Charge::Unresolved]);
}

#[test]
fn false_policy_preserves_genuine_handle_resume_and_finished_generation_retry() {
    for finished in [false, true] {
        let inner: Arc<dyn Provider> = if finished {
            Arc::new(FailsOnce { submits: AtomicU64::new(0), polls: AtomicU64::new(0) })
        } else {
            Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 2 })
        };
        let provider = NoReplay::new(inner);
        let (runner, budget) = budgeted(Some(1_000_000));
        runner.register(JobId(1), priced(provider.clone(), Some(100_000)));
        assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
        assert_eq!(block_on(runner.run(Start { attempt: 2, ..job(1) }, CancelToken::default())), Outcome::Done);
        assert_eq!(provider.invocations.load(Ordering::SeqCst), if finished { 2 } else { 1 });
        assert_eq!(spent(&budget), if finished { 200_000 } else { 100_000 });
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn false_policy_stops_actual_audited_local_build_failure() {
    use nodecraft_providers::llm::{ApiKey, Dialect, Endpoint, LlmClient};
    let http = Arc::new(nodecraft_providers::ReqwestHttp::new().unwrap());
    // Invalid header construction fails before execute; no listener, credentials or network call.
    let llm = LlmClient::new(
        Endpoint { id: "local-build".into(), dialect: Dialect::OpenAi, base_url: "http://127.0.0.1:1".into(), key: Some(ApiKey::new("public\ninvalid")) },
        http,
    );
    let provider = NoReplay::new(Arc::new(llm));
    let (runner, budget) = budgeted(Some(1_000_000));
    let mut queued = priced(provider.clone(), Some(100_000));
    queued.job.inputs = vec![nodecraft_providers::JobInput::Text { role: "user".into(), text: "public fixture".into() }];
    runner.register(JobId(1), queued);
    let runner = Arc::new(runner);
    let (_, events) = drive(runner.clone());
    assert_eq!(provider.invocations.load(Ordering::SeqCst), 1);
    assert_eq!(events.iter().filter(|event| matches!(event, Event::Retrying { .. })).count(), 0);
    assert_eq!(spent(&budget), 0, "terminal local build proof releases immediately");
    // Host settlement must preserve the immediate release.
    assert!(statuses(&budget).is_empty());
    runner.settle(JobId(1));
    assert_eq!(spent(&budget), 0);
    assert!(statuses(&budget).is_empty());
}

#[test]
fn cancellation_before_and_during_submit_remains_cancelled_with_correct_charge() {
    for before in [true, false] {
        let lost = LostReply::new(ErrorKind::Network);
        lost.ready.store(false, Ordering::SeqCst);
        let provider = NoReplay::new(lost.clone());
        let (runner, budget) = budgeted(Some(1_000_000));
        runner.register(JobId(1), priced(provider, Some(100_000)));
        let cancel = CancelToken::default();
        if before {
            cancel.cancel();
        }
        let mut work = runner.run(job(1), cancel.clone());
        if !before {
            let waker = futures::task::noop_waker();
            assert!(work.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_pending());
            cancel.cancel();
        }
        assert_eq!(block_on(work), Outcome::Cancelled);
        assert_eq!(lost.calls.load(Ordering::SeqCst), u64::from(!before));
        assert_eq!(spent(&budget), if before { 0 } else { 100_000 });
        assert_eq!(statuses(&budget), if before { vec![] } else { vec![Charge::Unresolved] });
    }
}

#[test]
fn registration_replacement_cannot_change_captured_attempt_policy() {
    let old = LostReply::new(ErrorKind::Network);
    old.ready.store(false, Ordering::SeqCst);
    let (runner, budget) = budgeted(Some(1_000_000));
    runner.register(JobId(1), priced(NoReplay::new(old.clone()), Some(100_000)));
    let mut work = runner.run(job(1), CancelToken::default());
    let waker = futures::task::noop_waker();
    assert!(work.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_pending());
    let replacement = LostReply::new(ErrorKind::Network);
    let mut queued = priced(replacement.clone(), Some(10));
    queued.job.idempotency_key = "fresh".into();
    runner.register(JobId(1), queued);
    old.ready.store(true, Ordering::SeqCst);
    assert!(matches!(block_on(work), Outcome::Failed { transient:false, error } if error == old.error.to_string()));
    assert_eq!(replacement.calls.load(Ordering::SeqCst), 0);
    assert_eq!(statuses(&budget), [Charge::Unresolved]);
    assert_eq!(spent(&budget), 100_000);
}

#[test]
fn unknown_price_terminal_failure_does_not_invent_money_and_drop_stays_conservative() {
    let (runner, budget) = budgeted(None);
    runner.register(JobId(1), priced(NoReplay::new(LostReply::new(ErrorKind::Network)), None));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: false, .. }));
    assert!(statuses(&budget).is_empty());
    assert_eq!(spent(&budget), 0);
    let pending = LostReply::new(ErrorKind::Network);
    pending.ready.store(false, Ordering::SeqCst);
    runner.register(JobId(2), priced(NoReplay::new(pending.clone()), Some(100_000)));
    let mut work = runner.run(job(2), CancelToken::default());
    let waker = futures::task::noop_waker();
    assert!(work.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_pending());
    drop(work);
    runner.settle(JobId(2));
    assert_eq!(pending.calls.load(Ordering::SeqCst), 1);
    assert_eq!(statuses(&budget), [Charge::Unresolved]);
    assert_eq!(spent(&budget), 100_000);
}

#[test]
fn huggingface48_real_adapter_lost_reply_is_not_replayed_by_driver() {
    struct LostHttp(AtomicU64);
    impl nodecraft_providers::Http for LostHttp {
        fn send(&self, _: nodecraft_providers::HttpRequest) -> BoxFuture<'_, Result<nodecraft_providers::HttpResponse, ProviderError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(ProviderError::new(ErrorKind::Network, "public lost reply")) })
        }
    }
    let http = Arc::new(LostHttp(AtomicU64::new(0)));
    let provider =
        Arc::new(nodecraft_providers::huggingface::HuggingFace::new(&nodecraft_providers::secrets::Secret::new("public-fixture").unwrap(), http.clone()));
    let (runner, budget) = budgeted(Some(1_000_000));
    let mut queued = priced(provider, Some(100_000));
    queued.job.model = nodecraft_providers::huggingface::MODEL.into();
    queued.job.inputs = vec![nodecraft_providers::JobInput::Text { role: "prompt".into(), text: "public harbour".into() }];
    queued.job.params = Default::default();
    runner.register(JobId(1), queued);
    let (_, events) = drive(Arc::new(runner));
    assert_eq!(http.0.load(Ordering::SeqCst), 1);
    assert!(!events.iter().any(|event| matches!(event, Event::Retrying { .. })), "{events:?}");
    assert_eq!(statuses(&budget), [Charge::Unresolved]);
}

#[test]
fn gemini44_real_adapter_lost_reply_is_not_replayed_by_driver() {
    struct LostHttp(AtomicU64);
    impl nodecraft_providers::Http for LostHttp {
        fn send(&self, _: nodecraft_providers::HttpRequest) -> BoxFuture<'_, Result<nodecraft_providers::HttpResponse, ProviderError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(ProviderError::new(ErrorKind::Network, "public lost reply")) })
        }
    }
    let http = Arc::new(LostHttp(AtomicU64::new(0)));
    let provider =
        Arc::new(nodecraft_providers::gemini_llm::GeminiLlm::new(&nodecraft_providers::secrets::Secret::new("public-fixture").unwrap(), http.clone()));
    let (runner, budget) = budgeted(Some(1_000_000));
    let mut queued = priced(provider, Some(100_000));
    queued.job.model = nodecraft_providers::gemini_llm::MODEL.into();
    queued.job.inputs = vec![nodecraft_providers::JobInput::Text { role: "prompt".into(), text: "public harbour".into() }];
    queued.job.params = Default::default();
    runner.register(JobId(1), queued);
    let (_, events) = drive(Arc::new(runner));
    assert_eq!(http.0.load(Ordering::SeqCst), 1);
    assert!(!events.iter().any(|event| matches!(event, Event::Retrying { .. })), "{events:?}");
    assert_eq!(statuses(&budget), [Charge::Unresolved]);
}

#[test]
fn deepgram40_real_lost_reply_is_not_replayed_by_driver() {
    struct LostHttp(AtomicU64);
    impl nodecraft_providers::Http for LostHttp {
        fn send(&self, _: nodecraft_providers::HttpRequest) -> BoxFuture<'_, Result<nodecraft_providers::HttpResponse, ProviderError>> {
            panic!("no redirects")
        }
        fn send_no_redirect(&self, _: nodecraft_providers::HttpRequest) -> BoxFuture<'_, Result<nodecraft_providers::HttpResponse, ProviderError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(ProviderError::new(ErrorKind::Network, "public lost reply")) })
        }
    }
    let http = Arc::new(LostHttp(AtomicU64::new(0)));
    let provider = Arc::new(nodecraft_providers::deepgram::Deepgram::new(nodecraft_providers::secrets::Secret::new("public-fixture").unwrap(), http.clone()));
    let (runner, budget) = budgeted(Some(1_000_000));
    let mut queued = priced(provider, Some(100_000));
    queued.job.model = nodecraft_providers::deepgram::MODEL.into();
    queued.job.params = Default::default();
    queued.job.inputs = vec![nodecraft_providers::JobInput::Media {
        role: "audio".into(),
        mime: "audio/wav".into(),
        bytes: Arc::from(include_bytes!("../../../../providers/src/fal/fixtures/tone.wav").as_slice()),
    }];
    runner.register(JobId(1), queued);
    let (_, events) = drive(Arc::new(runner));
    assert_eq!(http.0.load(Ordering::SeqCst), 1);
    assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })));
    assert_eq!(statuses(&budget), [Charge::Unresolved]);
}

#[test]
fn deepgram40_real_refusals_release_charges_and_rate_limits_retry_safely() {
    struct Responses {
        replies: Mutex<std::collections::VecDeque<nodecraft_providers::HttpResponse>>,
        calls: AtomicU64,
    }
    impl nodecraft_providers::Http for Responses {
        fn send(&self, _: nodecraft_providers::HttpRequest) -> BoxFuture<'_, Result<nodecraft_providers::HttpResponse, ProviderError>> {
            panic!("no redirects")
        }
        fn send_no_redirect(&self, _: nodecraft_providers::HttpRequest) -> BoxFuture<'_, Result<nodecraft_providers::HttpResponse, ProviderError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let reply = self.replies.lock().unwrap().pop_front().expect("fixture response");
            Box::pin(async { Ok(reply) })
        }
    }
    for (status, code) in
        [(401, "INVALID_AUTH"), (402, "ASR_PAYMENT_REQUIRED"), (400, "Bad Request"), (422, "ASR_UNPROCESSABLE_ENTITY"), (429, "TOO_MANY_REQUESTS")]
    {
        let error = nodecraft_providers::HttpResponse {
            status,
            headers: vec![],
            body: serde_json::to_vec(&json!({"err_code":code,"err_msg":"Public fixture refusal"})).unwrap(),
        };
        let success = nodecraft_providers::HttpResponse {
            status: 200,
            headers: vec![],
            body: br#"{"results":{"channels":[{"alternatives":[{"transcript":"Recovered after rate limit"}]}]}}"#.to_vec(),
        };
        let http = Arc::new(Responses { replies: Mutex::new([error, success].into()), calls: AtomicU64::new(0) });
        let provider =
            Arc::new(nodecraft_providers::deepgram::Deepgram::new(nodecraft_providers::secrets::Secret::new("public-fixture").unwrap(), http.clone()));
        let (runner, budget) = budgeted(Some(1_000_000));
        let mut queued = priced(provider, Some(100_000));
        queued.job.model = nodecraft_providers::deepgram::MODEL.into();
        queued.job.params = Default::default();
        queued.job.inputs = vec![nodecraft_providers::JobInput::Media {
            role: "audio".into(),
            mime: "audio/wav".into(),
            bytes: Arc::from(include_bytes!("../../../../providers/src/fal/fixtures/tone.wav").as_slice()),
        }];
        runner.register(JobId(1), queued);
        let (scheduler, events) = drive(Arc::new(runner));
        if status == 429 {
            assert_eq!(http.calls.load(Ordering::SeqCst), 2);
            assert_eq!(events.iter().filter(|e| matches!(e, Event::Retrying { .. })).count(), 1);
            assert!(matches!(scheduler.state(JobId(1)), Some(JobState::Done)));
            assert_eq!(spent(&budget), 100_000);
            assert_eq!(statuses(&budget), [Charge::Actual]);
        } else {
            assert_eq!(http.calls.load(Ordering::SeqCst), 1);
            assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })));
            assert_eq!(spent(&budget), 0);
            assert!(statuses(&budget).is_empty());
        }
    }
}
