//! Disabled paid smoke (one SOUL 2 image at 720p, a few credits). Execution also requires separate
//! human spending authorization.
#![cfg(all(feature = "http", feature = "higgsfield", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    JobInput, JobProgress, Output, Provider, ProviderJob, ReqwestHttp, Submission,
    higgsfield::{Higgsfield, SOUL},
    secrets::Secret,
};
use std::{sync::Arc, time::Duration};
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid request; explicit human authorization and both environment guards required"]
async fn live_higgsfield() {
    if std::env::var("NODECRAFT_LIVE_HIGGSFIELD").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_HIGGSFIELD_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    // `<key id>:<key secret>`, as stored in the keychain.
    let key = Secret::new(&std::env::var("HIGGSFIELD_API_KEY").expect("provide an authorized key without logging it")).unwrap();
    let p = Higgsfield::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let j = ProviderJob {
        model: SOUL.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A small plain wooden cube on a white table.".into() }],
        idempotency_key: format!("nodecraft-live-{}", std::process::id()),
    };
    let quote = p.estimate(&j).await.unwrap();
    assert!(!quote.credits.is_empty());
    let Submission::Queued(handle) = p.submit(&j).await.unwrap() else { panic!("Higgsfield queues its requests") };
    for _ in 0..200 {
        match p.poll(&handle).await.unwrap() {
            JobProgress::Done(done) => {
                let [Output::Bytes { mime, bytes }] = done.outputs.as_slice() else { panic!("one image") };
                assert!(mime.starts_with("image/") && !bytes.is_empty());
                return;
            }
            JobProgress::Failed(error) => panic!("{error:?}"),
            _ => tokio::time::sleep(Duration::from_secs(3)).await,
        }
    }
    panic!("the request did not finish in ten minutes");
}
