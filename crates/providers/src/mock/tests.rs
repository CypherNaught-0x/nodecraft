//! These tests deliberately use the unit-test crate's local trait, never a dev facade.
use crate::{JobInput, Output, Provider, ProviderJob, Submission, mock::MockProvider};
use futures::executor::block_on;

#[test]
fn local_mock_implements_local_provider_and_preserves_accepted_identity() {
    let mock = MockProvider::standard();
    let local: &dyn Provider = &mock;
    let mut job = ProviderJob {
        model: "mock-text".into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "original".into() }],
        idempotency_key: "public-fixture".into(),
    };
    let Submission::Finished(original) = block_on(local.submit(&job)).unwrap() else { panic!("standard mock is synchronous") };
    assert!(matches!(original.outputs.first(), Some(Output::Text(text)) if text.contains("original")));
    job.model = "unknown-model".into();
    job.inputs.clear();
    let Submission::Finished(replay) = block_on(local.submit(&job)).unwrap() else { panic!("accepted result remains synchronous") };
    assert_eq!(replay, original);
    assert_eq!(mock.billed(), 1);
    assert_eq!(mock.requests().len(), 2);
}

#[test]
fn a_mock_context_window_refuses_only_longer_jobs_and_bills_nothing_for_them() {
    use crate::mock::{MockFailure, MockModel, MockOutput};
    let model = MockModel { fail: Some(MockFailure::ContextLimit(4)), ..MockModel::new(MockOutput::Text) };
    let mock = MockProvider::new("mock", [("m".to_owned(), model)]);
    let job = |text: &str, key: &str| ProviderJob {
        model: "m".into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: text.into() }],
        idempotency_key: key.into(),
    };
    // Seventeen bytes are five tokens at four bytes a token: one past the window.
    let refused = block_on(mock.submit(&job("seventeen bytes!!", "long"))).unwrap_err();
    assert!(refused.is_context_limit(), "{refused}");
    assert!(refused.message.contains("context window is 4 tokens; the request has 5"), "{refused}");
    assert_eq!(mock.billed(), 0, "a refused submit is not billed");
    assert!(matches!(block_on(mock.submit(&job("sixteen bytes!!!", "short"))), Ok(Submission::Finished(_))), "four tokens fit");
    assert_eq!(mock.billed(), 1);
}
