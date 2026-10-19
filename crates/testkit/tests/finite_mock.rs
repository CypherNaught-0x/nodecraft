use futures::executor::block_on;
use nodecraft_providers::{ErrorKind, Provider, ProviderJob, Submission};
use nodecraft_testkit::mock::{MockFailures, MockModel, MockOperation, MockOutput, MockProvider, MockScript};

#[test]
fn legacy_reexport_exposes_the_same_additive_finite_api() {
    let provider = MockProvider::scripted(
        "mock",
        [("m".into(), MockModel::new(MockOutput::Text))],
        [MockOperation {
            id: "op".into(),
            model: "m".into(),
            sent_keys: vec!["key".into()],
            script: MockScript { submit_failures: Some(MockFailures { count: 1, kind: ErrorKind::Network }), ..Default::default() },
        }],
    )
    .unwrap();
    let job = ProviderJob { model: "m".into(), params: Default::default(), inputs: vec![], idempotency_key: "key".into() };
    assert!(block_on(provider.submit(&job)).is_err());
    assert_eq!(provider.billed(), 0);
    assert!(matches!(block_on(provider.submit(&job)).unwrap(), Submission::Finished(_)));
    assert_eq!(provider.billed(), 1);
}
