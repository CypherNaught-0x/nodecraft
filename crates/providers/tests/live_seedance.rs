//! Disabled paid smoke: Seedance 2.0 Mini, 480p, 4 seconds (at most about 0.15 USD). Execution
//! also requires separate human spending authorization.
#![cfg(all(feature = "http", feature = "seedance", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    JobInput, JobProgress, Output, Provider, ProviderJob, ReqwestHttp, Submission,
    secrets::Secret,
    seedance::{Seedance, V2_0_MINI},
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid request; explicit human authorization and both environment guards required"]
async fn live_seedance() {
    if std::env::var("NODECRAFT_LIVE_SEEDANCE").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_SEEDANCE_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("ARK_API_KEY").expect("provide an authorized key without logging it")).unwrap();
    let p = Seedance::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let mut j = ProviderJob {
        model: V2_0_MINI.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A small paper boat drifting on a calm pond.".into() }],
        idempotency_key: "explicit-live-no-server-dedup".into(),
    };
    j.params.insert("resolution".into(), json!("480p"));
    j.params.insert("duration_seconds".into(), json!("4"));
    let Submission::Queued(handle) = p.submit(&j).await.unwrap() else { panic!("Seedance queues its tasks") };
    for _ in 0..120 {
        match p.poll(&handle).await.unwrap() {
            JobProgress::Done(done) => {
                let [Output::Bytes { mime, bytes }] = done.outputs.as_slice() else { panic!("one video") };
                assert_eq!(mime, "video/mp4");
                assert!(bytes.len() > 1_000);
                assert!(done.usage.output_tokens.is_some_and(|n| n > 0));
                return;
            }
            JobProgress::Failed(error) => panic!("{error:?}"),
            _ => tokio::time::sleep(Duration::from_secs(5)).await,
        }
    }
    panic!("the task did not finish in ten minutes");
}
