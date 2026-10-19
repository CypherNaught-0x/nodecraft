use nodecraft_provider_testkit::mock::MockProvider;
use nodecraft_providers::Provider;
use std::sync::Arc;

#[test]
fn facade_mock_implements_the_engines_existing_provider_dependency() {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::standard());
    assert_eq!(provider.id(), "mock");
}
