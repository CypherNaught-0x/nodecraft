//! Disabled paid smoke (about 0.40 USD). Execution also requires separate human spending
//! authorization.
#![cfg(all(feature = "http", feature = "meshy", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    JobInput, JobProgress, Output, Provider, ProviderJob, ReqwestHttp, Submission,
    meshy::{Meshy, TEXT},
    secrets::Secret,
};
use std::{sync::Arc, time::Duration};
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid request; explicit human authorization and both environment guards required"]
async fn live_meshy() {
    if std::env::var("NODECRAFT_LIVE_MESHY").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_MESHY_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("MESHY_API_KEY").expect("provide an authorized key without logging it")).unwrap();
    let p = Meshy::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let j = ProviderJob {
        model: TEXT.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A small plain wooden cube.".into() }],
        idempotency_key: "explicit-live-no-server-dedup".into(),
    };
    let Submission::Queued(handle) = p.submit(&j).await.unwrap() else { panic!("Meshy queues its tasks") };
    for _ in 0..200 {
        match p.poll(&handle).await.unwrap() {
            JobProgress::Done(done) => {
                let [Output::Bytes { mime, bytes }] = done.outputs.as_slice() else { panic!("one model") };
                assert_eq!(mime, "model/gltf-binary");
                assert!(bytes.starts_with(b"glTF"));
                return;
            }
            JobProgress::Failed(error) => panic!("{error:?}"),
            _ => tokio::time::sleep(Duration::from_secs(3)).await,
        }
    }
    panic!("the task did not finish in ten minutes");
}
