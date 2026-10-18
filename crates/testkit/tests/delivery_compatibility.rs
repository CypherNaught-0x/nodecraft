//! Legacy public implementors keep source compatibility and conservative constructor evidence.
use futures::executor::block_on;
use nodecraft_providers::{BoxFuture, CancelSignal, ErrorKind, ProviderJob, RunLimits, run_to_completion};
use nodecraft_testkit::{
    golden::{InstantClock, NeverCancelled},
    mock::{MockFailure, MockModel, MockOutput, MockProvider},
};
struct Cancelled;
impl CancelSignal for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}
#[test]
fn actual_mock_lost_reply_accepts_during_construction_and_remains_unknown() {
    for kind in [ErrorKind::Auth, ErrorKind::UnsupportedInput, ErrorKind::Network] {
        let p =
            MockProvider::new("mock", [("model".into(), MockModel { polls: 1, fail: Some(MockFailure::LostReply(kind)), ..MockModel::new(MockOutput::Text) })]);
        let job = ProviderJob { model: "model".into(), params: Default::default(), inputs: vec![], idempotency_key: "public-fixture".into() };
        let failure = block_on(run_to_completion(&p, &job, &NeverCancelled, &InstantClock::default(), RunLimits::default(), |_| {})).unwrap_err();
        assert_eq!(failure.error.kind, kind);
        assert!(failure.uncertain);
        assert_eq!(p.billed(), 1);
        assert_eq!(p.requests().len(), 1);
        let before = p.requests().len();
        let failure = block_on(run_to_completion(&p, &job, &Cancelled, &InstantClock::default(), RunLimits::default(), |_| {})).unwrap_err();
        assert!(!failure.uncertain);
        assert_eq!(p.requests().len(), before);
    }
}
