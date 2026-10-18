use futures::executor::block_on;
use nodecraft_provider_testkit::{
    mock::MockProvider,
    providers::{Provider, ProviderJob, Submission},
};

#[test]
fn exec_tests_can_submit_through_the_provider_only_facade() {
    let mock = MockProvider::standard();
    let provider: &dyn Provider = &mock;
    let job = ProviderJob { model: "mock-text".into(), params: Default::default(), inputs: vec![], idempotency_key: "public-fixture".into() };
    assert!(matches!(block_on(provider.submit(&job)).unwrap(), Submission::Finished(_)));
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(mock.billed(), 1);
}
