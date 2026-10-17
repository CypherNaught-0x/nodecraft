use futures::executor::block_on;
use nodecraft_providers::{CancelSignal, Clock, ErrorKind, JobInput, Output, Provider, ProviderJob, RunLimits, Submission, run_to_completion};
use nodecraft_testkit::mock::{MockFailure, MockModel, MockOutput, MockProvider};
use nodecraft_types::{Currency, Money};
use std::sync::atomic::{AtomicU64, Ordering};

struct Never;
impl CancelSignal for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> nodecraft_providers::BoxFuture<'_, ()> {
        Box::pin(futures::future::pending())
    }
}
#[derive(Default)]
struct Instant(AtomicU64);
impl Clock for Instant {
    fn sleep(&self, ms: u64) -> nodecraft_providers::BoxFuture<'static, ()> {
        self.0.fetch_add(ms, Ordering::SeqCst);
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn job(model: &str, prompt: &str, key: &str) -> ProviderJob {
    let mut params = serde_json::Map::new();
    params.insert("style".into(), serde_json::json!("ink"));
    ProviderJob { model: model.into(), params, inputs: vec![JobInput::Text { role: "prompt".into(), text: prompt.into() }], idempotency_key: key.into() }
}

fn run(provider: &MockProvider, job: &ProviderJob) -> Result<Vec<Output>, ErrorKind> {
    block_on(run_to_completion(provider, job, &Never, &Instant::default(), RunLimits::default(), |_| {})).map(|done| done.outputs).map_err(|f| f.error.kind)
}

#[test]
fn results_are_deterministic_and_follow_the_request() {
    let provider = MockProvider::standard();
    let a = run(&provider, &job("mock-text", "a fox", "k1")).unwrap();
    assert_eq!(a, run(&provider, &job("mock-text", "a fox", "k2")).unwrap(), "same request, same result");
    assert_ne!(a, run(&provider, &job("mock-text", "an owl", "k3")).unwrap());
    let Output::Text(text) = &a[0] else { panic!("text model returns text") };
    assert!(text.starts_with("mock-text: a fox #"), "{text}");
    let image = run(&provider, &job("mock-image", "a fox", "k4")).unwrap();
    assert!(matches!(&image[0], Output::Bytes { mime, .. } if mime == "image/x-nodecraft-mock"));
    assert_eq!(provider.requests().len(), 4);
    assert_eq!(provider.billed(), 4);
}

#[test]
fn a_retry_under_the_same_key_is_billed_once_and_polls_are_counted() {
    let cost = Money::new(Currency::USD, 40_000);
    let provider = MockProvider::new("mock", [("slow".to_owned(), MockModel { polls: 3, cost: Some(cost), ..MockModel::new(MockOutput::Text) })]);
    let request = job("slow", "a fox", "k1");
    let done = block_on(run_to_completion(&provider, &request, &Never, &Instant::default(), RunLimits::default(), |_| {})).unwrap();
    assert_eq!(done.usage.cost, Some(cost));
    assert_eq!(provider.polls(), 3);
    // Resubmitting under the same key returns the same job.
    let again = block_on(provider.submit(&request)).unwrap();
    assert!(matches!(again, Submission::Queued(h) if h.remote_id == "mock-0"));
    assert_eq!(provider.billed(), 1);
}

#[test]
fn scripted_failures_surface_where_they_are_scripted() {
    let model = |fail| MockModel { polls: 1, fail: Some(fail), ..MockModel::new(MockOutput::Text) };
    let provider = MockProvider::new(
        "mock",
        [
            ("refuses".to_owned(), model(MockFailure::Submit(ErrorKind::Auth))),
            ("drops".to_owned(), model(MockFailure::Poll(ErrorKind::Network))),
            ("fails".to_owned(), model(MockFailure::Job(ErrorKind::JobFailed))),
        ],
    );
    assert_eq!(run(&provider, &job("refuses", "x", "a")), Err(ErrorKind::Auth));
    assert_eq!(run(&provider, &job("drops", "x", "b")), Err(ErrorKind::Network));
    assert_eq!(run(&provider, &job("fails", "x", "c")), Err(ErrorKind::JobFailed));
    assert_eq!(run(&provider, &job("nope", "x", "d")), Err(ErrorKind::UnsupportedInput));
}
