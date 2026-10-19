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
