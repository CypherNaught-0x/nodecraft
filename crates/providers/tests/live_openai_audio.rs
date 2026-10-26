//! Opt-in paid smoke, never run automatically. Speech endpoint sunsets 2027-01-06.
#![cfg(all(feature = "http", feature = "openai-audio", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    JobInput, Output, Provider, ProviderJob, ReqwestHttp, Submission,
    openai_audio::{OpenAiAudio, SPEECH, TRANSCRIBE},
    secrets::Secret,
};
use std::sync::Arc;
#[tokio::test(flavor = "current_thread")]
#[ignore = "two paid OpenAI audio calls; explicit authorization and both environment guards required"]
async fn live_openai_audio() {
    if std::env::var("NODECRAFT_LIVE_OPENAI_AUDIO").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_OPENAI_AUDIO_ACKNOWLEDGE_COST").as_deref() != Ok("1")
    {
        return;
    }
    let key = Secret::new(&std::env::var("OPENAI_API_KEY").expect("authorized API key")).unwrap();
    let provider = OpenAiAudio::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let job = ProviderJob {
        model: SPEECH.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A paper boat.".into() }],
        idempotency_key: "manual-live-no-retry".into(),
    };
    let Submission::Finished(speech) = provider.submit(&job).await.unwrap() else { panic!("synchronous") };
    let [Output::Bytes { mime, bytes }] = speech.outputs.as_slice() else { panic!("one speech output") };
    let job = ProviderJob { model: TRANSCRIBE.into(), inputs: vec![JobInput::Media { role: "audio".into(), mime: mime.clone(), bytes: bytes.clone() }], ..job };
    let Submission::Finished(text) = provider.submit(&job).await.unwrap() else { panic!("synchronous") };
    assert!(matches!(text.outputs.as_slice(),[Output::Text(t)] if !t.trim().is_empty()));
}
