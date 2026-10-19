//! Ignored paid smoke; never selected in CI and never run without separate spending approval.
#![cfg(all(feature = "http", feature = "gemini-images", not(target_arch = "wasm32")))]
use nodecraft_providers::gemini_images::{GeminiImages, LITE};
use nodecraft_providers::secrets::Secret;
use nodecraft_providers::{JobInput, Provider, ProviderJob, ReqwestHttp, Submission};
use std::sync::Arc;
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid Gemini image request; requires explicit human authorization and both guards"]
async fn live_gemini_images() {
    if std::env::var("NODECRAFT_LIVE_GEMINI_IMAGES").as_deref() != Ok("1")
        || std::env::var("NODECRAFT_LIVE_GEMINI_IMAGES_ACKNOWLEDGE_COST").as_deref() != Ok("1")
    {
        return;
    }
    let key = Secret::new(&std::env::var("GEMINI_API_KEY").expect("supply an authorized key without logging it")).unwrap();
    let p = GeminiImages::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let mut job = ProviderJob {
        model: LITE.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A small plain blue square on a white background.".into() }],
        idempotency_key: "explicit-live-no-dedup-promise".into(),
    };
    let Submission::Finished(done) = p.submit(&job).await.unwrap() else { panic!("synchronous output") };
    assert!(!done.outputs.is_empty());
    if std::env::var("NODECRAFT_LIVE_GEMINI_IMAGES_EDIT").as_deref() == Ok("1") {
        job.inputs.push(JobInput::Media {
            role: "image".into(),
            mime: "image/jpeg".into(),
            bytes: Arc::from(include_bytes!("fixtures/gemini_images/original-square.jpg").as_slice()),
        });
        let Submission::Finished(done) = p.submit(&job).await.unwrap() else { panic!("synchronous edit") };
        assert!(!done.outputs.is_empty());
    }
}
