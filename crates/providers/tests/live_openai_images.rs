//! Disabled paid smoke test. Requires fresh human spending authorization in addition to these
//! environment guards. Never run in CI; synthetic fixtures cover edits/masks/options/N.
#![cfg(all(feature = "http", feature = "openai-images", not(target_arch = "wasm32")))]
use nodecraft_providers::openai_images::{FLARE, OpenAiImages};
use nodecraft_providers::secrets::Secret;
use nodecraft_providers::{JobInput, Provider, ProviderJob, ReqwestHttp, Submission};
use std::sync::Arc;
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid OpenAI Images request; explicit human authorization and both environment guards required"]
async fn live_openai_images() {
    if std::env::var("NODECRAFT_LIVE_OPENAI_IMAGES").as_deref() != Ok("1")
        || std::env::var("NODECRAFT_LIVE_OPENAI_IMAGES_ACKNOWLEDGE_COST").as_deref() != Ok("1")
    {
        return;
    }
    // Credentials are read only after both guards and explicit ignored-test selection.
    let key = Secret::new(&std::env::var("OPENAI_API_KEY").expect("provide an authorized key without logging it")).unwrap();
    let provider = OpenAiImages::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let job = ProviderJob {
        model: FLARE.into(),
        params: serde_json::json!({"quality":"low","size":"1024x1024","n":1}).as_object().unwrap().clone(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A small plain blue circle on a white background.".into() }],
        idempotency_key: "explicit-live-image-smoke-no-dedup-promise".into(),
    };
    let Submission::Finished(done) = provider.submit(&job).await.unwrap() else { panic!("synchronous image result") };
    assert_eq!(done.outputs.len(), 1);
}
