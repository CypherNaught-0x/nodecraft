//! Disabled paid smoke: one short Flash v2.5 sentence (about 0.002 USD). Run it only with
//! explicit human spending authorization and `NODECRAFT_LIVE_ELEVENLABS=1`.
#![cfg(all(feature = "http", feature = "elevenlabs", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    JobInput, Output, Provider, ProviderJob, ReqwestHttp, Submission,
    elevenlabs::{ElevenLabs, FLASH_V2_5},
    secrets::Secret,
};
use std::sync::Arc;
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid request; explicit human authorization and NODECRAFT_LIVE_ELEVENLABS=1 required"]
async fn live_elevenlabs() {
    if std::env::var("NODECRAFT_LIVE_ELEVENLABS").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("ELEVENLABS_API_KEY").expect("provide an authorized key without logging it")).unwrap();
    let p = ElevenLabs::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let text = "NodeCraft speaks.";
    let j = ProviderJob {
        model: FLASH_V2_5.into(),
        params: serde_json::json!({"voice": "EXAVITQu4vr4xnSDxMaL"}).as_object().cloned().unwrap(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: text.into() }],
        idempotency_key: "explicit-live-no-server-dedup".into(),
    };
    let Submission::Finished(c) = p.submit(&j).await.unwrap() else { panic!("synchronous") };
    let [Output::Bytes { mime, bytes }] = c.outputs.as_slice() else { panic!("one MP3") };
    assert_eq!(mime, "audio/mpeg");
    assert!(bytes.len() > 1000, "{} bytes", bytes.len());
    assert_eq!(c.usage.billable_units.map(|u| u.millionths()), Some(text.chars().count() as u64 * 1_000_000));
}
