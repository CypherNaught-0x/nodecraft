//! Explicit opt-in paid smoke; no private recording is read.
#![cfg(all(feature = "http", feature = "deepgram", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    JobInput, Output, Provider, ProviderJob, ReqwestHttp, Submission,
    deepgram::{Deepgram, MODEL},
    secrets::Secret,
};
use std::sync::Arc;
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid request; explicit spending authorization and NODECRAFT_LIVE_DEEPGRAM=1 required"]
async fn live_deepgram40() {
    if std::env::var("NODECRAFT_LIVE_DEEPGRAM").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("DEEPGRAM_API_KEY").unwrap()).unwrap();
    let p = Deepgram::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let job = ProviderJob {
        model: MODEL.into(),
        params: Default::default(),
        inputs: vec![JobInput::Media {
            role: "audio".into(),
            mime: "audio/wav".into(),
            bytes: Arc::from(include_bytes!("../src/fal/fixtures/tone.wav").as_slice()),
        }],
        idempotency_key: "explicit-live-no-dedup".into(),
    };
    let Submission::Finished(done) = p.submit(&job).await.unwrap() else { panic!("sync") };
    assert!(matches!(done.outputs.as_slice(), [Output::Text(_)]));
}
