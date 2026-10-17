use futures::executor::block_on;
use nodecraft_providers::{ErrorKind, JobInput, Output, Provider, ProviderJob, RunLimits, Submission, run_to_completion};
use nodecraft_testkit::mock::{MockFailure, MockModel, MockOutput, MockProvider};
use nodecraft_types::{Currency, Money};

use nodecraft_testkit::golden::{InstantClock as Instant, NeverCancelled as Never};

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
    // Real signatures, so hosts sniff it as the media it claims to be.
    assert!(matches!(&image[0], Output::Bytes { bytes, .. } if nodecraft_media_sniff_png(bytes)));
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

#[test]
fn a_lost_reply_bills_once_and_the_retry_gets_the_job() {
    let provider = MockProvider::new(
        "mock",
        [("flaky".to_owned(), MockModel { polls: 1, fail: Some(MockFailure::LostReply(ErrorKind::Network)), ..MockModel::new(MockOutput::Text) })],
    );
    let request = job("flaky", "a fox", "k1");
    assert_eq!(run(&provider, &request), Err(ErrorKind::Network), "the reply was lost");
    assert_eq!(provider.billed(), 1, "but the job was accepted");
    // Retrying under the same key resumes the same job; no second charge.
    assert!(run(&provider, &request).is_ok());
    assert_eq!(provider.billed(), 1);
}

#[test]
fn cancelled_jobs_stay_cancelled_and_sync_retries_return_the_original_result() {
    let provider = MockProvider::new("mock", [("slow".to_owned(), MockModel { polls: 5, ..MockModel::new(MockOutput::Image) })]);
    let Submission::Queued(handle) = block_on(provider.submit(&job("slow", "x", "k1"))).unwrap() else { panic!("queued") };
    block_on(provider.cancel(&handle)).unwrap();
    assert_eq!(provider.cancelled(), vec![handle.remote_id.clone()]);
    assert!(matches!(block_on(provider.poll(&handle)), Ok(nodecraft_providers::JobProgress::Failed(e)) if e.kind == ErrorKind::Cancelled));
    // A model with no polls fails at submit when scripted to fail.
    let sync_fail =
        MockProvider::new("mock", [("m".to_owned(), MockModel { fail: Some(MockFailure::Job(ErrorKind::JobFailed)), ..MockModel::new(MockOutput::Text) })]);
    assert_eq!(run(&sync_fail, &job("m", "x", "k")), Err(ErrorKind::JobFailed));
    // A resubmit under the same key gets the first result, even with other inputs.
    let standard = MockProvider::standard();
    let first = run(&standard, &job("mock-text", "a fox", "same")).unwrap();
    assert_eq!(run(&standard, &job("mock-text", "an owl", "same")).unwrap(), first);
}

fn nodecraft_media_sniff_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}
