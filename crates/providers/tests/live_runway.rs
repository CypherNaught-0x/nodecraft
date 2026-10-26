//! Opt-in paid smoke: Gen-4.5 text-to-video, 2 seconds, 0.24 USD before tax.
#![cfg(all(feature = "http", feature = "runway", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    JobInput, JobProgress, Output, Provider, ProviderJob, ReqwestHttp, Submission,
    runway::{GEN4_5, Runway},
    secrets::Secret,
};
use std::{sync::Arc, time::Duration};
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid request; explicit spending authorization and both environment guards required"]
async fn live_runway() {
    if std::env::var("NODECRAFT_LIVE_RUNWAY").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_RUNWAY_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("RUNWAYML_API_SECRET").expect("provide an authorized key without logging it")).unwrap();
    let p = Runway::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let job = ProviderJob {
        model: GEN4_5.into(),
        params: serde_json::json!({"duration_seconds":"2","ratio":"1280:720"}).as_object().unwrap().clone(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A paper boat drifting on a calm pond.".into() }],
        idempotency_key: "explicit-live-no-server-dedup".into(),
    };
    let Submission::Queued(handle) = p.submit(&job).await.unwrap() else { panic!("queued") };
    for _ in 0..120 {
        match p.poll(&handle).await.unwrap() {
            JobProgress::Done(done) => {
                assert!(matches!(done.outputs.as_slice(),[Output::Bytes {mime,bytes}] if mime=="video/mp4" && bytes.len()>1000));
                return;
            }
            JobProgress::Failed(error) => panic!("{error}"),
            _ => tokio::time::sleep(Duration::from_secs(5)).await,
        }
    }
    let _ = p.cancel(&handle).await;
    panic!("Runway task did not finish within ten minutes");
}
