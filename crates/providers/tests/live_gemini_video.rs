//! Disabled paid smoke (a few seconds of 360p video, under 1 USD). Execution also requires
//! separate human spending authorization.
#![cfg(all(feature = "http", feature = "gemini-video", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    JobInput, JobProgress, Output, Provider, ProviderJob, ReqwestHttp, Submission,
    gemini_video::{GeminiVideo, OMNI_FLASH},
    secrets::Secret,
};
use std::{sync::Arc, time::Duration};
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid request; explicit human authorization and both environment guards required"]
async fn live_gemini_video() {
    if std::env::var("NODECRAFT_LIVE_GEMINI_VIDEO").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_GEMINI_VIDEO_ACKNOWLEDGE_COST").as_deref() != Ok("1")
    {
        return;
    }
    let key = Secret::new(&std::env::var("GEMINI_API_KEY").expect("provide an authorized key without logging it")).unwrap();
    let p = GeminiVideo::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let mut j = ProviderJob {
        model: OMNI_FLASH.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "[0-3s] A marble rolls slowly across a plain wooden table.".into() }],
        idempotency_key: "explicit-live-no-server-dedup".into(),
    };
    j.params.insert("resolution".into(), "360p".into());
    let outputs = match p.submit(&j).await.unwrap() {
        Submission::Finished(done) => done.outputs,
        Submission::Queued(handle) => 'poll: {
            for _ in 0..120 {
                match p.poll(&handle).await.unwrap() {
                    JobProgress::Done(done) => break 'poll done.outputs,
                    JobProgress::Failed(error) => panic!("{error:?}"),
                    _ => tokio::time::sleep(Duration::from_secs(5)).await,
                }
            }
            panic!("the file was not ready in ten minutes");
        }
    };
    let [Output::Bytes { mime, bytes }] = outputs.as_slice() else { panic!("one video") };
    assert_eq!(mime, "video/mp4");
    assert_eq!(bytes.get(4..8), Some(&b"ftyp"[..]));
}
