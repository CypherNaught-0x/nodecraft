use futures::executor::block_on;
use nodecraft_providers::{ErrorKind, JobInput, Output, Provider, ProviderJob, RunLimits, Submission, run_to_completion};
use nodecraft_testkit::mock::{MockFailure, MockModel, MockOutput, MockProvider};
use nodecraft_types::{Currency, Money};

use nodecraft_testkit::golden::{InstantClock as Instant, NeverCancelled as Never};

#[test]
fn legacy_mock_path_is_the_provider_crates_same_type() {
    let moved: nodecraft_providers::mock::MockProvider = MockProvider::standard();
    let legacy: MockProvider = moved;
    let provider: &dyn Provider = &legacy;
    assert_eq!(provider.id(), "mock");
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
    // Real signatures, so hosts sniff it as the media it claims to be.
    assert!(matches!(&image[0], Output::Bytes { bytes, .. } if starts_with_png_signature(bytes)));
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

fn starts_with_png_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

fn identity_provider() -> MockProvider {
    let model = |output, polls, fail| MockModel { output, polls, cost: Some(Money::new(Currency::USD, 40_000)), fail };
    MockProvider::new(
        "mock",
        [
            ("queued".into(), model(MockOutput::Image, 3, None)),
            ("sync".into(), model(MockOutput::Text, 0, None)),
            ("refuse".into(), model(MockOutput::Text, 0, Some(MockFailure::Submit(ErrorKind::Auth)))),
            ("poll-fail".into(), model(MockOutput::Text, 0, Some(MockFailure::Poll(ErrorKind::Network)))),
            ("job-fail".into(), model(MockOutput::Text, 0, Some(MockFailure::Job(ErrorKind::JobFailed)))),
            ("queued-fail".into(), model(MockOutput::Image, 3, Some(MockFailure::Poll(ErrorKind::Network)))),
            ("lost-queued".into(), model(MockOutput::Image, 3, Some(MockFailure::LostReply(ErrorKind::Network)))),
            ("lost-sync".into(), model(MockOutput::Text, 0, Some(MockFailure::LostReply(ErrorKind::Network)))),
        ],
    )
}

fn queued(provider: &MockProvider, request: &ProviderJob) -> nodecraft_providers::JobHandle {
    let Submission::Queued(handle) = block_on(provider.submit(request)).unwrap() else { panic!("original accepted job must remain queued") };
    handle
}

fn finished(provider: &MockProvider, request: &ProviderJob) -> nodecraft_providers::Completed {
    let Submission::Finished(done) = block_on(provider.submit(request)).unwrap() else { panic!("original synchronous result must remain finished") };
    done
}

fn changed_requests(key: &str, original_model: &str) -> Vec<ProviderJob> {
    let mut changed_params = job(original_model, "original", key);
    changed_params.params.insert("style".into(), serde_json::json!("different"));
    let mut changed_media = job(original_model, "original", key);
    changed_media.inputs = vec![JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: std::sync::Arc::from(b"different media".as_slice()) }];
    let mut requests = vec![job(original_model, "original", key), changed_params, job(original_model, "different text", key), changed_media];
    requests.extend(["queued", "sync", "unknown", "refuse", "poll-fail", "job-fail", "queued-fail"].map(|model| job(model, "different", key)));
    requests
}

#[test]
fn accepted_queued_identity_wins_over_all_retry_requests_and_keeps_poll_progress() {
    use nodecraft_providers::JobProgress;
    let provider = identity_provider();
    let original = job("queued", "original", "key");
    let handle = queued(&provider, &original);
    assert!(matches!(block_on(provider.poll(&handle)).unwrap(), JobProgress::Running { fraction: Some(f), .. } if (f - 1.0/3.0).abs() < 0.001));
    let retries = changed_requests("key", "queued");
    for retry in &retries {
        assert_eq!(queued(&provider, retry), handle);
    }
    assert!(matches!(block_on(provider.poll(&handle)).unwrap(), JobProgress::Running { fraction: Some(f), .. } if (f - 2.0/3.0).abs() < 0.001));
    let JobProgress::Done(done) = block_on(provider.poll(&handle)).unwrap() else { panic!("third original poll completes") };
    let expected = run(&identity_provider(), &original).unwrap();
    assert_eq!(done.outputs, expected);
    assert!(matches!(done.outputs.first(), Some(Output::Bytes { mime, .. }) if mime == "image/png"));
    for retry in &retries {
        assert_eq!(queued(&provider, retry), handle);
    }
    assert!(matches!(block_on(provider.poll(&handle)).unwrap(), JobProgress::Done(again) if again == done));
    assert_eq!(provider.requests().len(), 1 + 2 * retries.len());
    assert_eq!(provider.billed(), 1);
    assert_eq!(provider.polls(), 4);
}

#[test]
fn accepted_sync_identity_wins_over_model_params_inputs_and_failure_scripts() {
    let provider = identity_provider();
    let original = job("sync", "original", "key");
    let done = finished(&provider, &original);
    let retries = changed_requests("key", "sync");
    for retry in &retries {
        assert_eq!(finished(&provider, retry), done);
    }
    assert!(matches!(done.outputs.first(), Some(Output::Text(text)) if text.starts_with("sync: original #")));
    assert_eq!(done.usage.cost, Some(Money::new(Currency::USD, 40_000)));
    assert_eq!(provider.requests().len(), 1 + retries.len());
    assert_eq!(provider.billed(), 1);
    assert_eq!(provider.polls(), 0);
    assert!(provider.cancelled().is_empty());
}

#[test]
fn initial_refusals_do_not_reserve_identity() {
    for model in ["unknown", "refuse", "poll-fail", "job-fail"] {
        let provider = identity_provider();
        assert!(block_on(provider.submit(&job(model, "refused", "key"))).is_err());
        assert_eq!(provider.billed(), 0);
        let done = finished(&provider, &job("sync", "original", "key"));
        assert!(matches!(done.outputs.first(), Some(Output::Text(text)) if text.starts_with("sync: original #")));
        assert_eq!(provider.billed(), 1);
        assert_eq!(provider.requests().len(), 2);
    }
}

#[test]
fn lost_replies_replay_original_queued_or_sync_acceptance_after_changed_retry() {
    use nodecraft_providers::JobProgress;
    let provider = identity_provider();
    let request = job("lost-queued", "original", "key");
    assert_eq!(block_on(provider.submit(&request)).unwrap_err().kind, ErrorKind::Network);
    let handle = queued(&provider, &job("sync", "changed", "key"));
    assert_eq!(handle.remote_id, "mock-0");
    assert_eq!(queued(&provider, &request), handle);
    for _ in 0..2 {
        assert!(matches!(block_on(provider.poll(&handle)).unwrap(), JobProgress::Running { .. }));
    }
    let JobProgress::Done(done) = block_on(provider.poll(&handle)).unwrap() else { panic!("original queued work completes") };
    let expected = run(&identity_provider(), &job("queued", "original", "other")).unwrap();
    // The digest includes the original model; compare with a separate accepted original job.
    let oracle = identity_provider();
    assert!(block_on(oracle.submit(&request)).is_err());
    let original_handle = queued(&oracle, &request);
    let mut original_done = None;
    for _ in 0..3 {
        if let JobProgress::Done(value) = block_on(oracle.poll(&original_handle)).unwrap() {
            original_done = Some(value);
        }
    }
    assert_eq!(Some(done.clone()), original_done);
    assert_ne!(done.outputs, expected, "retry must not replace the original model identity");
    assert_eq!((provider.billed(), provider.requests().len(), provider.polls()), (1, 3, 3));
    let provider = identity_provider();
    let request = job("lost-sync", "original", "key");
    assert_eq!(block_on(provider.submit(&request)).unwrap_err().kind, ErrorKind::Network);
    let done = finished(&provider, &job("queued", "changed", "key"));
    assert_eq!(finished(&provider, &request), done);
    assert!(matches!(done.outputs.first(), Some(Output::Text(text)) if text.starts_with("lost-sync: original #")));
    assert_eq!((provider.billed(), provider.requests().len(), provider.polls()), (1, 3, 0));
}

#[test]
fn cancelled_identity_survives_changed_retry_without_resurrecting_work() {
    use nodecraft_providers::JobProgress;
    let provider = identity_provider();
    let handle = queued(&provider, &job("queued", "original", "key"));
    block_on(provider.cancel(&handle)).unwrap();
    let retries = changed_requests("key", "queued");
    for retry in &retries {
        assert_eq!(queued(&provider, retry), handle);
        assert!(matches!(block_on(provider.poll(&handle)).unwrap(), JobProgress::Failed(error) if error.kind == ErrorKind::Cancelled));
    }
    assert_eq!(provider.cancelled(), vec![handle.remote_id]);
    assert_eq!(provider.billed(), 1);
    assert_eq!(provider.requests().len(), 1 + retries.len());
    assert_eq!(provider.polls(), retries.len() as u64);
}
