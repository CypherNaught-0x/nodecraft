//! Ignored paid smoke; never selected in CI and never run without separate spending approval.
//! One 1K Gemini 3.1 Flash-Lite Image (about 0.034 USD at Google's listed price).
#![cfg(all(feature = "http", feature = "google-gemini-images", not(target_arch = "wasm32")))]
use nodecraft_providers::google_cloud::gemini_images::{FLASH_LITE, GoogleGeminiImages};
use nodecraft_providers::google_cloud::{compact, system_clock};
use nodecraft_providers::{JobInput, Output, Provider, ProviderJob, ReqwestHttp, Submission};
use std::sync::Arc;
#[tokio::test(flavor = "current_thread")]
#[ignore = "a paid Gemini image request; requires explicit human authorization and both guards"]
async fn live_google_gemini_images() {
    if std::env::var("NODECRAFT_LIVE_GOOGLE_GEMINI_IMAGES").as_deref() != Ok("1")
        || std::env::var("NODECRAFT_LIVE_GOOGLE_GEMINI_IMAGES_ACKNOWLEDGE_COST").as_deref() != Ok("1")
    {
        return;
    }
    let path = std::env::var("GOOGLE_GEMINI_IMAGES_KEY_FILE").expect("the path of an authorized service-account key file");
    let secret = compact(&std::fs::read_to_string(path).expect("the key file can be read")).expect("a service-account key file");
    let p = GoogleGeminiImages::new(&secret, Arc::new(ReqwestHttp::new().unwrap()), system_clock()).unwrap();
    let job = ProviderJob {
        model: FLASH_LITE.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A paper boat on a rain puddle, watercolor.".into() }],
        idempotency_key: "explicit-live-no-dedup-promise".into(),
    };
    let Submission::Finished(done) = p.submit(&job).await.unwrap() else { panic!("images are synchronous") };
    assert!(done.outputs.iter().all(|o| matches!(o, Output::Bytes { mime, .. } if mime.starts_with("image/"))));
    assert!(!done.outputs.is_empty());
}
