//! Disabled paid smoke (about 0.20 USD). Execution also requires separate human spending
//! authorization.
#![cfg(all(feature = "http", feature = "tripo", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    JobInput, JobProgress, Output, Provider, ProviderJob, ReqwestHttp, Submission,
    secrets::Secret,
    tripo::{TEXT, Tripo},
};
use std::{sync::Arc, time::Duration};
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid request; explicit human authorization and both environment guards required"]
async fn live_tripo() {
    if std::env::var("NODECRAFT_LIVE_TRIPO").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_TRIPO_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("TRIPO_API_KEY").expect("provide an authorized key without logging it")).unwrap();
    let p = Tripo::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let j = ProviderJob {
        model: TEXT.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A small plain wooden cube.".into() }],
        idempotency_key: "explicit-live-no-server-dedup".into(),
    };
    let Submission::Queued(handle) = p.submit(&j).await.unwrap() else { panic!("Tripo queues its tasks") };
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
