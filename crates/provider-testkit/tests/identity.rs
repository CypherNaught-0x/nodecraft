use nodecraft_provider_testkit::{mock::MockProvider, providers::Provider};

#[test]
fn facade_reexports_the_external_provider_trait_and_mock_type() {
    let mock: nodecraft_provider_testkit::providers::mock::MockProvider = MockProvider::standard();
    let provider: &dyn Provider = &mock;
    assert_eq!(provider.id(), "mock");
}
