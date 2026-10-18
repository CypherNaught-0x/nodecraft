use nodecraft_provider_testkit::{mock::MockProvider, providers::Provider};

#[test]
fn lower_layer_can_use_the_mock_without_a_provider_dependency_or_self_cycle() {
    let mock = MockProvider::standard();
    let provider: &dyn Provider = &mock;
    assert_eq!(provider.id(), "mock");
}
