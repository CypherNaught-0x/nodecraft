//! Explicit paid checks of Luma's live aliases through fal. Never run by ordinary CI.
#![cfg(all(feature = "fal", feature = "http", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    BoxFuture, CancelSignal, Clock, Completed, JobInput, Output, ProviderJob, ReqwestHttp, RunLimits,
    fal::{Fal, LUMA_PHOTON, LUMA_RAY},
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
async fn generate(model: &str) -> Completed {
    assert_eq!(std::env::var("NODECRAFT_LIVE_LUMA").as_deref(), Ok("1"));
    assert_eq!(std::env::var("NODECRAFT_LIVE_LUMA_ACKNOWLEDGE_COST").as_deref(), Ok("1"));
    let key = Secret::new(&std::env::var("FAL_KEY").expect("explicit test-only FAL_KEY required")).unwrap();
    let provider = Fal::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let job = ProviderJob {
        model: model.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A blue paper circle turning slowly on a plain white background".into() }],
        idempotency_key: format!("explicit-paid-luma-{model}"),
    };
    run_to_completion(&provider, &job, &Never, &RealClock(Instant::now()), RunLimits { deadline_ms: 600_000, ..Default::default() }, |_| {}).await.unwrap()
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid 0.50 USD Ray video before tax; explicit spending authorization required"]
async fn luma26_live_ray_video() {
    let result = generate(LUMA_RAY).await;
    assert!(matches!(&result.outputs[..],[Output::Bytes {mime,..}] if mime == "video/mp4"));
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid Photon image at 0.019 USD/MP, dimensions and total unknown; explicit spending authorization required"]
async fn luma26_live_photon_image() {
    let result = generate(LUMA_PHOTON).await;
    assert!(matches!(&result.outputs[..],[Output::Bytes {mime,..}] if matches!(mime.as_str(),"image/png"|"image/jpeg"|"image/webp")));
}
