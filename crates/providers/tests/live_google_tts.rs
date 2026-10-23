//! Ignored paid smoke; never selected in CI and never run without separate spending approval.
//! One Standard voice line of 39 characters (about 0.0002 USD at Google's listed price, and inside
//! the monthly free tier) and one short Gemini 2.5 Flash TTS line (well under 0.01 USD).
#![cfg(all(feature = "http", feature = "google-tts", not(target_arch = "wasm32")))]
use nodecraft_providers::google_cloud::tts::{GEMINI_FLASH, GoogleTts, STANDARD};
use nodecraft_providers::google_cloud::{compact, system_clock};
use nodecraft_providers::{JobInput, Output, Provider, ProviderJob, ReqwestHttp, Submission};
use std::sync::Arc;
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid Google TTS requests; requires explicit human authorization and both guards"]
async fn live_google_tts() {
    if std::env::var("NODECRAFT_LIVE_GOOGLE_TTS").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_GOOGLE_TTS_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    let path = std::env::var("GOOGLE_TTS_KEY_FILE").expect("the path of an authorized service-account key file");
    let secret = compact(&std::fs::read_to_string(path).expect("the key file can be read")).expect("a service-account key file");
    let p = GoogleTts::new(&secret, Arc::new(ReqwestHttp::new().unwrap()), system_clock()).unwrap();
    for (model, voice) in [(STANDARD, "en-US-Standard-C"), (GEMINI_FLASH, "Kore")] {
        let mut job = ProviderJob {
            model: model.into(),
            params: Default::default(),
            inputs: vec![JobInput::Text { role: "prompt".into(), text: "A paper boat drifts down a rain gutter.".into() }],
            idempotency_key: "explicit-live-no-dedup-promise".into(),
        };
        job.params.insert("voice".into(), voice.into());
        let Submission::Finished(done) = p.submit(&job).await.unwrap() else { panic!("speech is synchronous") };
        assert!(matches!(done.outputs.as_slice(), [Output::Bytes { mime, .. }] if mime == "audio/mpeg"), "{model}");
    }
}
