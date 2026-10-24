//! Disabled paid smoke (one Kling Image 2.1 picture, a few Kling units). Execution also requires
//! separate human spending authorization.
#![cfg(all(feature = "http", feature = "kling", not(target_arch = "wasm32")))]
use nodecraft_providers::{JobInput, JobProgress, Output, Provider, ProviderJob, ReqwestHttp, Submission, kling::Kling, secrets::Secret};
use std::{sync::Arc, time::Duration};
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid request; explicit human authorization and both environment guards required"]
async fn live_kling() {
    if std::env::var("NODECRAFT_LIVE_KLING").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_KLING_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("KLING_API_KEY").expect("provide an authorized key without logging it")).unwrap();
    let p = Kling::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let j = ProviderJob {
        model: "kling-v2-1".into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A single red apple on a plain white table".into() }],
        idempotency_key: "nodecraft:live:kling".into(),
    };
    let Submission::Queued(handle) = p.submit(&j).await.unwrap() else { panic!("Kling always queues a task") };
    for _ in 0..120 {
        match p.poll(&handle).await.unwrap() {
            JobProgress::Done(done) => {
                let [Output::Bytes { mime, bytes }] = done.outputs.as_slice() else { panic!("one image") };
                assert!(mime.starts_with("image/") && !bytes.is_empty());
                return;
            }
            JobProgress::Failed(error) => panic!("{error:?}"),
            _ => tokio::time::sleep(Duration::from_secs(5)).await,
        }
    }
    panic!("the image was not ready in ten minutes");
}
