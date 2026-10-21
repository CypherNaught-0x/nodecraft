//! A documented refusal of a paid create proves no job, so it's retried like any transient
//! failure under the same charge; a 5xx or a lost reply stays possibly billed (#631).
use super::*;
use nodecraft_exec::scheduler::{Event, JobSpec, JobState, Limits, RetryPolicy, Scheduler};
use nodecraft_providers::comfy_cloud::ComfyCloud;
use nodecraft_providers::tripo::{TEXT, Tripo};
use nodecraft_providers::{Http, HttpRequest, HttpResponse, secrets::Secret};
use std::collections::VecDeque;

struct Transport {
    replies: Mutex<VecDeque<Result<HttpResponse, ProviderError>>>,
    sent: AtomicU64,
    keys: Mutex<Vec<String>>,
}
impl Http for Transport {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("legacy send must not run")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.sent.fetch_add(1, Ordering::SeqCst);
        if let Some((_, key)) = r.headers.iter().find(|(name, _)| name == "Idempotency-Key") {
            self.keys.lock().unwrap().push(key.clone());
        }
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { reply })
    }
}
fn reply(status: u16, code: i64) -> Result<HttpResponse, ProviderError> {
    let body = serde_json::to_vec(&serde_json::json!({"code": code, "message": "fixture"})).unwrap();
    Ok(HttpResponse { status, headers: vec![], body })
}

struct Inline(AtomicU64);
impl Runtime for Inline {
    fn spawn(&self, mut task: TaskFuture<()>) {
        let waker = futures::task::noop_waker();
        assert!(task.as_mut().poll(&mut std::task::Context::from_waker(&waker)).is_ready(), "fixture needs no asynchronous progress");
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        self.0.fetch_add(ms, Ordering::SeqCst);
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Runs one Tripo text task through the real driver against `replies`; returns the scheduler,
/// its events, the requests sent, the time slept and the budget, after the host settled the job.
fn run(replies: Vec<Result<HttpResponse, ProviderError>>) -> (Scheduler, Vec<Event>, u64, u64, JobBudget) {
    let (scheduler, events, http, slept, budget) = drive(replies, |http| {
        let job = ProviderJob {
            model: TEXT.into(),
            params: Default::default(),
            inputs: vec![nodecraft_providers::JobInput::Text { role: "prompt".into(), text: "a red chair".into() }],
            idempotency_key: String::new(),
        };
        (Arc::new(Tripo::new(Secret::new("tripo-fixture-key-not-real").unwrap(), http)), job)
    });
    (scheduler, events, http.sent.load(Ordering::SeqCst), slept, budget)
}
fn drive(
    replies: Vec<Result<HttpResponse, ProviderError>>,
    build: impl Fn(Arc<Transport>) -> (Arc<dyn Provider>, ProviderJob),
) -> (Scheduler, Vec<Event>, Arc<Transport>, u64, JobBudget) {
    let http = Arc::new(Transport { replies: Mutex::new(replies.into()), sent: AtomicU64::new(0), keys: Mutex::default() });
    let (provider, job) = build(http.clone());
    assert!(!provider.retry_ambiguous_submit(), "only proved refusals may be sent again");
    let (runner, budget) = budgeted(Some(1_000_000));
    let mut queued = priced(provider, Some(200_000));
    queued.job = job;
    runner.register(JobId(1), queued);
    let mut scheduler = Scheduler::new(Limits::default());
    scheduler
        .add(vec![JobSpec {
            id: JobId(1),
            deps: vec![],
            lane: Lane::new("fixture"),
            retry: RetryPolicy { max_attempts: 3, base_delay_ms: 1_000, max_delay_ms: 4_000 },
            idempotency_key: "nodecraft:r:1".into(),
        }])
        .unwrap();
    let (driver, handle) = nodecraft_exec::driver::driver(scheduler);
    drop(handle);
    let runtime = Arc::new(Inline(AtomicU64::new(0)));
    let runner = Arc::new(runner);
    let mut events = vec![];
    let scheduler = block_on(driver.run(runner.clone(), runtime.clone(), |event| events.push(event)));
    // A last transient failure leaves its reservation for the host, as for any job (#517).
    runner.settle(JobId(1));
    let slept = runtime.0.load(Ordering::SeqCst);
    (scheduler, events, http, slept, budget)
}
fn retries(events: &[Event]) -> usize {
    events.iter().filter(|event| matches!(event, Event::Retrying { .. })).count()
}
fn charges(budget: &JobBudget) -> Vec<Charge> {
    budget.log.lock().unwrap().entries().map(|entry| entry.status).collect()
}

#[test]
fn a_refused_create_is_retried_with_back_off_and_charged_once() {
    // Rate limited twice, then a 5xx: a third attempt that may have created the task.
    let (scheduler, events, sent, slept, budget) = run(vec![reply(429, 2000), reply(429, 1007), reply(500, 1000)]);
    assert_eq!(sent, 3);
    assert_eq!(retries(&events), 2, "{events:?}");
    assert!(slept >= 1_000, "the retries backed off ({slept} ms)");
    assert!(matches!(scheduler.state(JobId(1)), Some(JobState::Failed { .. })));
    assert_eq!(spent(&budget), 200_000, "the 5xx may have billed: one charge kept");
    assert_eq!(charges(&budget), [Charge::Unresolved]);
}

#[test]
fn refusals_to_the_end_release_the_charge() {
    let (scheduler, events, sent, _, budget) = run(vec![reply(429, 2000), reply(429, 2000), reply(429, 2000)]);
    assert_eq!((sent, retries(&events)), (3, 2));
    assert!(matches!(scheduler.state(JobId(1)), Some(JobState::Failed { .. })));
    assert_eq!(spent(&budget), 0, "no task was ever created");
    // Too little credit isn't transient: one try, nothing kept.
    let (_, events, sent, _, budget) = run(vec![reply(403, 2010)]);
    assert_eq!((sent, retries(&events)), (1, 0));
    assert_eq!(spent(&budget), 0);
}

#[test]
fn a_lost_or_failed_create_reply_stays_possibly_billed_and_is_not_sent_again() {
    for last in [reply(503, 2000), reply(500, 1000), Err(ProviderError::new(ErrorKind::Timeout, "lost")), reply(200, 2010)] {
        let (_, events, sent, _, budget) = run(vec![last]);
        assert_eq!((sent, retries(&events)), (1, 0), "{events:?}");
        assert_eq!(spent(&budget), 200_000);
        assert_eq!(charges(&budget), [Charge::Unresolved]);
    }
}

#[test]
fn a_full_comfy_queue_is_retried_under_the_same_key_and_charged_once() {
    const OBJECT_INFO: &str = r#"{
      "CLIPTextEncode": {"input": {"required": {"text": ["STRING", {"multiline": true}]}}, "output": ["CONDITIONING"]},
      "SaveText": {"input": {"required": {"text": ["STRING", {}]}}, "output": [], "output_node": true}
    }"#;
    let full = || {
        let body = serde_json::to_vec(&serde_json::json!({"error": {"code": "queue_full", "message": "busy"}})).unwrap();
        Ok(HttpResponse { status: 429, headers: vec![], body })
    };
    let lost = Err(ProviderError::new(ErrorKind::Network, "reset"));
    let catalog = Ok(HttpResponse { status: 200, headers: vec![], body: OBJECT_INFO.into() });
    let (scheduler, events, http, slept, budget) = drive(vec![catalog, full(), full(), lost], |http| {
        let workflow = serde_json::json!({"workflow": {
            "1": {"class_type": "CLIPTextEncode", "inputs": {"text": "a red fox"}},
            "2": {"class_type": "SaveText", "inputs": {"text": "x"}}
        }});
        let job = ProviderJob {
            model: "comfy-cloud:workflow".into(),
            params: workflow.as_object().cloned().unwrap(),
            inputs: vec![],
            idempotency_key: String::new(),
        };
        (Arc::new(ComfyCloud::new(Secret::new("comfy-fixture-key-not-real").unwrap(), http, Arc::new(|| 0))), job)
    });
    assert_eq!(http.sent.load(Ordering::SeqCst), 4, "the catalog once, then three creates");
    assert_eq!(retries(&events), 2, "{events:?}");
    assert!(slept >= 1_000, "the retries backed off ({slept} ms)");
    let keys = http.keys.lock().unwrap().clone();
    assert_eq!(keys.len(), 3);
    assert!(keys.iter().all(|key| key == &keys[0]), "every attempt reuses the key Comfy released: {keys:?}");
    assert!(matches!(scheduler.state(JobId(1)), Some(JobState::Failed { .. })));
    assert_eq!(spent(&budget), 200_000, "the lost reply may hide a job: one charge kept");
    assert_eq!(charges(&budget), [Charge::Unresolved]);
}
