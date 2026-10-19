//! Explicit paid opt-in only. Ordinary CI compiles but never runs this test body.
#![cfg(all(feature = "fal", feature = "http", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    BoxFuture, CancelSignal, Clock, JobInput, ProviderJob, ReqwestHttp, RunLimits,
    fal::{FLUX, Fal},
    run_to_completion,
    secrets::Secret,
};
use std::{sync::Arc, time::Instant};
struct Never;
impl CancelSignal for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(futures::future::pending())
    }
}
struct RealClock(Instant);
impl Clock for RealClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
    fn sleep(&self, ms: u64) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(std::time::Duration::from_millis(ms)))
    }
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid fal call; fresh human authorization and both explicit cost opt-ins required"]
async fn fal_small_flux_image() {
    assert_eq!(std::env::var("NODECRAFT_LIVE_FAL").as_deref(), Ok("1"));
    assert_eq!(std::env::var("NODECRAFT_LIVE_FAL_ACKNOWLEDGE_COST").as_deref(), Ok("1"));
    let key = Secret::new(&std::env::var("FAL_KEY").expect("explicit test-only FAL_KEY required")).unwrap();
    let provider = Fal::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let job = ProviderJob {
        model: FLUX.into(),
        params: serde_json::from_value(serde_json::json!({"width":512,"height":512})).unwrap(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A blue circle on a white background".into() }],
        idempotency_key: "explicit-paid-live-fal".into(),
    };
    let completed =
        run_to_completion(&provider, &job, &Never, &RealClock(Instant::now()), RunLimits { deadline_ms: 120_000, ..Default::default() }, |_| {}).await.unwrap();
    assert_eq!(completed.outputs.len(), 1);
    assert!(completed.usage.cost.is_none());
}
