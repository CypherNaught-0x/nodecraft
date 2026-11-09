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

/// Topaz's own cost opt-in prevents enabling it as a side effect of a different live smoke.
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid Topaz call; fresh human authorization and explicit cost opt-ins required"]
async fn topaz37_small_precision_image() {
    assert_eq!(std::env::var("NODECRAFT_LIVE_FAL").as_deref(), Ok("1"));
    assert_eq!(std::env::var("NODECRAFT_LIVE_FAL_TOPAZ").as_deref(), Ok("1"));
    assert_eq!(std::env::var("NODECRAFT_LIVE_FAL_ACKNOWLEDGE_COST").as_deref(), Ok("1"));
    let key = Secret::new(&std::env::var("FAL_KEY").expect("explicit test-only FAL_KEY required")).unwrap();
    let provider = Fal::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let image = nodecraft_media::encode_png(64, 64, &vec![120; 64 * 64 * 4], &Default::default()).unwrap();
    let job = ProviderJob {
        model: nodecraft_providers::fal::TOPAZ_IMAGE.into(),
        params: Default::default(),
        inputs: vec![JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: image.into() }],
        idempotency_key: "explicit-paid-live-topaz37".into(),
    };
    let done =
        run_to_completion(&provider, &job, &Never, &RealClock(Instant::now()), RunLimits { deadline_ms: 300_000, ..Default::default() }, |_| {}).await.unwrap();
    let [nodecraft_providers::Output::Bytes { mime, bytes }] = done.outputs.as_slice() else { panic!("one complete output") };
    assert_eq!(mime, "image/png");
    let meta = nodecraft_media::probe_image(bytes, &Default::default()).unwrap();
    assert_eq!((meta.width, meta.height), (128, 128));
}
