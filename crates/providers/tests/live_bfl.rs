//! Explicit paid smoke, with a public prompt and bounded polling. Never enabled in CI.
#![cfg(all(feature = "http", feature = "bfl", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    JobInput, JobProgress, Output, Provider, ProviderJob, ReqwestHttp, Submission,
    bfl::{Bfl, MODEL},
    secrets::Secret,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid request; explicit spending authorization and NODECRAFT_LIVE_BFL=1 required"]
async fn live_bfl33() {
    if std::env::var("NODECRAFT_LIVE_BFL").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("BFL_API_KEY").unwrap()).unwrap();
    let provider = Bfl::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let job = ProviderJob {
        model: MODEL.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A paper boat on a calm pond, watercolor".into() }],
        idempotency_key: "explicit-live-no-dedup".into(),
    };
    let Submission::Queued(handle) = provider.submit(&job).await.unwrap() else { panic!("queue") };
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        match provider.poll(&handle).await.unwrap() {
            JobProgress::Done(done) => {
                assert!(matches!(done.outputs.as_slice(),[Output::Bytes{mime,..}] if mime=="image/png"));
                break;
            }
            JobProgress::Failed(error) => panic!("generation failed: {:?}", error.kind),
            _ => {
                assert!(Instant::now() < deadline, "poll deadline exceeded; submitted work may still be billed");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}
