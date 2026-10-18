use super::*;
use nodecraft_exec::scheduler::{Event, JobSpec, JobState, Limits, RetryPolicy, Scheduler};
use nodecraft_providers::{
    JobInput, ReqwestHttp,
    llm::{ApiKey, Dialect, Endpoint, LlmClient},
};
use std::{
    net::TcpListener,
    time::{Duration, Instant},
};

// Scheduling allowance for loaded CI; these tests assert delivery/accounting, not latency.
const FIXTURE_TIMEOUT: Duration = Duration::from_secs(30);
const RUN_DEADLINE_MS: u64 = 20_000;

struct TokioRuntime(Instant);
impl Runtime for TokioRuntime {
    fn spawn(&self, task: TaskFuture<()>) {
        tokio::spawn(task);
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(ms)))
    }
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}
fn client(url: &str, bad: bool) -> Arc<LlmClient> {
    Arc::new(LlmClient::new(
        Endpoint {
            id: "native-build".into(),
            dialect: Dialect::OpenAi,
            base_url: url.into(),
            key: Some(ApiKey::new(if bad { "public-header\nprivate-key-marker" } else { "public-fixture" })),
        },
        Arc::new(ReqwestHttp::new().unwrap()),
    ))
}
fn queued(provider: Arc<dyn Provider>) -> QueuedJob {
    let mut queued = priced(provider, Some(100_000));
    queued.job.model = "model".into();
    queued.job.inputs = vec![JobInput::Text { role: "user".into(), text: "public fixture".into() }];
    queued
}
fn runner(runtime: Arc<TokioRuntime>) -> (Arc<ProviderRunner>, JobBudget) {
    let (_, budget) = budgeted(Some(1_000_000));
    let runner = ProviderRunner::new(
        runtime,
        RunLimits { deadline_ms: RUN_DEADLINE_MS, ..RunLimits::default() },
        Arc::new(|_, _| panic!("failed builder must not produce output")),
        Arc::new(|_, _, _| {}),
    )
    .with_budget(budget.clone());
    (Arc::new(runner), budget)
}
fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/private-url-marker", listener.local_addr().unwrap());
    (listener, url)
}
// Reads one bounded HTTP request and either closes its reply or returns a valid LLM body.
async fn serve(listener: TcpListener, body: Option<&'static [u8]>) -> Vec<u8> {
    tokio::time::timeout(FIXTURE_TIMEOUT, async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            stream.readable().await.unwrap();
            let mut buf = [0; 4096];
            match stream.try_read(&mut buf) {
                Ok(0) => panic!("fixture client closed before full request"),
                Ok(n) => bytes.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(e) => panic!("fixture read: {e}"),
            }
            assert!(bytes.len() <= 65536);
            if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                let header = std::str::from_utf8(&bytes[..end]).unwrap();
                let length = header
                    .lines()
                    .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|n| n.trim().parse::<usize>().unwrap()))
                    .unwrap_or(0);
                assert!(length <= 65536);
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        if let Some(body) = body {
            let mut reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
            reply.extend_from_slice(body);
            let mut sent = 0;
            while sent < reply.len() {
                stream.writable().await.unwrap();
                match stream.try_write(&reply[sent..]) {
                    Ok(0) => panic!("fixture write stopped"),
                    Ok(n) => sent += n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("fixture write: {e}"),
                }
            }
        }
        bytes
    })
    .await
    .expect("bounded native fixture")
}
async fn drive(runner: Arc<ProviderRunner>, runtime: Arc<TokioRuntime>, mut on_event: impl FnMut(&Event)) -> (Scheduler, Vec<Event>) {
    let mut scheduler = Scheduler::new(Limits::default());
    scheduler
        .add(vec![JobSpec {
            id: JobId(1),
            deps: vec![],
            lane: Lane::new("native-build"),
            retry: RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 2 },
            idempotency_key: "nodecraft:r:1".into(),
        }])
        .unwrap();
    let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
    drop(handle);
    let mut events = vec![];
    let scheduler = tokio::time::timeout(
        FIXTURE_TIMEOUT,
        driver.run(runner, runtime, |event| {
            on_event(&event);
            events.push(event);
        }),
    )
    .await
    .expect("bounded Driver");
    (scheduler, events)
}
#[tokio::test(flavor = "current_thread")]
async fn fresh_native_builder_failure_is_terminal_and_releases_only_proven_reservation() {
    let (listener, url) = listener();
    let runtime = Arc::new(TokioRuntime(Instant::now()));
    let (runner, budget) = runner(runtime.clone());
    runner.register(JobId(1), queued(client(&url, true)));
    let (scheduler, events) = drive(runner.clone(), runtime, |_| {}).await;
    assert!(matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock), "bad build never reached TCP listener");
    // Positive control reaches THIS same listener under the entered reactor, so observation is live.
    let positive = client(&url, false);
    let job = queued(positive.clone()).job;
    let (done, wire) = tokio::join!(positive.submit(&job), serve(listener, Some(br#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#)));
    assert!(matches!(done.unwrap(), Submission::Finished(_)));
    assert!(wire.starts_with(b"POST "));
    assert_eq!(events.iter().filter(|e| matches!(e, Event::Started { .. })).count(), 1, "one actual Driver attempt");
    assert!(!events.iter().any(|e| matches!(e, Event::Retrying { .. })), "{events:?}");
    assert!(
        matches!(scheduler.state(JobId(1)),Some(JobState::Failed{error})if error.contains("HTTP request could not be constructed")&&!error.contains("private-"))
    );
    assert_eq!(spent(&budget), 0);
    assert_eq!(budget.log.lock().unwrap().entries().count(), 0);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, 0);
    {
        let state = runner.state.lock().unwrap();
        assert!(state.reserved.is_empty());
        assert!(state.metadata.is_empty());
        assert!(state.running.is_empty());
    }
    runner.settle(JobId(1));
    runner.forget(JobId(1));
    assert_eq!(spent(&budget), 0);
}
#[tokio::test(flavor = "current_thread")]
async fn earlier_actual_ambiguous_attempt_survives_later_native_builder_failure() {
    let (listener, url) = listener();
    let runtime = Arc::new(TokioRuntime(Instant::now()));
    let (runner, budget) = runner(runtime.clone());
    runner.register(JobId(1), queued(client(&url, false)));
    let replacement = runner.clone();
    let retry_url = url.clone();
    let ((scheduler, events), wire) = tokio::join!(
        drive(runner.clone(), runtime, move |event| {
            if matches!(event, Event::Retrying { .. }) {
                replacement.register(JobId(1), queued(client(&retry_url, true)));
            }
        }),
        serve(listener, None)
    );
    assert!(wire.starts_with(b"POST "));
    assert!(matches!(scheduler.state(JobId(1)), Some(JobState::Failed { .. })));
    assert_eq!(events.iter().filter(|e| matches!(e, Event::Started { .. })).count(), 2);
    assert_eq!(events.iter().filter(|e| matches!(e, Event::Retrying { .. })).count(), 1);
    assert_eq!(spent(&budget), 100_000);
    assert_eq!(budget.log.lock().unwrap().entries().map(|e| e.status).collect::<Vec<_>>(), [Charge::Unresolved]);
    runner.settle(JobId(1));
    runner.forget(JobId(1));
    assert_eq!(spent(&budget), 100_000);
}
