use futures::executor::block_on;
use nodecraft_provider_testkit::{
    mock::MockProvider,
    providers::{Provider, ProviderJob, Submission},
};

#[test]
fn lower_layer_mock_submission_uses_the_same_external_provider_types() {
    let mock = MockProvider::standard();
    let provider: &dyn Provider = &mock;
    let job = ProviderJob { model: "mock-text".into(), params: Default::default(), inputs: vec![], idempotency_key: "public-fixture".into() };
    assert!(matches!(block_on(provider.submit(&job)).unwrap(), Submission::Finished(_)));
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(mock.billed(), 1);
}
