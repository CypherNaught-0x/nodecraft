//! Disabled first-party smoke: one chosen call, only after explicit human spending authorization.
#![cfg(all(feature = "http", feature = "stability", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    BoxFuture, CancelSignal, Clock, JobInput, Output, ProviderJob, ReqwestHttp, RunLimits, run_to_completion,
    secrets::Secret,
    stability::{AUDIO, CORE, FAST_3D, Stability},
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
#[ignore = "paid request ($0.03/$0.08/$0.10); explicit human authorization and NODECRAFT_LIVE_STABILITY=1 required"]
async fn live_stability_one_chosen_family() {
    if std::env::var("NODECRAFT_LIVE_STABILITY").as_deref() != Ok("1") {
        return;
    }
    let model = std::env::var("NODECRAFT_LIVE_STABILITY_MODEL").unwrap_or_else(|_| CORE.into());
    assert!([CORE, AUDIO, FAST_3D].contains(&model.as_str()));
    let key = Secret::new(&std::env::var("STABILITY_API_KEY").expect("provide an authorized key without logging it")).unwrap();
    let provider = Stability::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let inputs = if model == FAST_3D {
        let bytes = nodecraft_media::encode_png(64, 64, &vec![160; 64 * 64 * 4], &Default::default()).unwrap();
        vec![JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: bytes.into() }]
    } else {
        vec![JobInput::Text { role: "prompt".into(), text: "A plain blue paper boat".into() }]
    };
    let job = ProviderJob {
        model: model.clone(),
        params: if model == AUDIO { serde_json::json!({"duration_seconds":1}).as_object().unwrap().clone() } else { Default::default() },
        inputs,
        idempotency_key: "explicit-live-no-server-dedup".into(),
    };
    let done = run_to_completion(&provider, &job, &Never, &RealClock(Instant::now()), RunLimits::default(), |_| {}).await.unwrap();
    let [Output::Bytes { mime, bytes }] = done.outputs.as_slice() else { panic!("one typed result") };
    assert_eq!(
        mime,
        match model.as_str() {
            CORE => "image/png",
            AUDIO => "audio/wav",
            _ => "model/gltf-binary",
        }
    );
    assert!(!bytes.is_empty());
}
