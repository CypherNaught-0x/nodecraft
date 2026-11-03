//! Paid opt-in smoke only. Ordinary CI compiles this file without running the test.
#![cfg(all(feature = "magnific", feature = "http", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    BoxFuture, CancelSignal, Clock, JobInput, Output, ProviderJob, ReqwestHttp, RunLimits,
    magnific::{Magnific, PRECISION_V2},
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
#[ignore = "paid Magnific call; fresh human authorization and explicit unpriced-cost acknowledgment required"]
async fn magnific38_small_precision_image() {
    assert_eq!(std::env::var("NODECRAFT_LIVE_MAGNIFIC").as_deref(), Ok("1"));
    assert_eq!(std::env::var("NODECRAFT_LIVE_MAGNIFIC_ACKNOWLEDGE_UNPRICED_COST").as_deref(), Ok("1"));
    let key = Secret::new(&std::env::var("MAGNIFIC_API_KEY").expect("explicit test-only MAGNIFIC_API_KEY required")).unwrap();
    let image = nodecraft_media::encode_png(256, 256, &vec![120; 256 * 256 * 4], &Default::default()).unwrap();
    let provider = Magnific::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let job = ProviderJob {
        model: PRECISION_V2.into(),
        params: Default::default(),
        inputs: vec![JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: image.into() }],
        idempotency_key: "explicit-paid-live-magnific38".into(),
    };
    let completed =
        run_to_completion(&provider, &job, &Never, &RealClock(Instant::now()), RunLimits { deadline_ms: 300_000, ..Default::default() }, |_| {}).await.unwrap();
    let [Output::Bytes { mime, bytes }] = completed.outputs.as_slice() else { panic!("one complete raster result") };
    assert!(matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp"));
    let metadata = nodecraft_media::probe_image(bytes, &Default::default()).unwrap();
    assert_eq!((metadata.width, metadata.height), (512, 512));
    assert!(completed.usage.cost.is_none(), "the API does not supply a verified currency charge");
}
