//! Paid opt-in only; never executed by ordinary CI. Synthetic tests cover the adapter contract.
#![cfg(all(feature = "http", feature = "replicate", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    JobInput, JobProgress, Provider, ProviderJob, ReqwestHttp, Submission,
    replicate::{HELLO, Replicate},
    secrets::Secret,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid Replicate request; explicit human authorization and both environment guards required"]
async fn live_replicate() {
    if std::env::var("NODECRAFT_LIVE_REPLICATE").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_REPLICATE_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("REPLICATE_API_TOKEN").expect("provide authorized key without logging it")).unwrap();
    let p = Replicate::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let j = ProviderJob {
        model: HELLO.into(),
        params: serde_json::Map::new(),
        inputs: vec![JobInput::Text { role: "text".into(), text: "Hello NodeCraft".into() }],
        idempotency_key: "explicit-live-no-dedup-promise".into(),
    };
    let Submission::Queued(h) = p.submit(&j).await.unwrap() else { panic!("prediction handle") };
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        match p.poll(&h).await.unwrap() {
            JobProgress::Done(done) => {
                assert_eq!(done.outputs.len(), 1);
                return;
            }
            JobProgress::Failed(e) => panic!("prediction failed: {e}"),
            _ => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    let _ = p.cancel(&h).await;
    panic!("live prediction exceeded bounded wait");
}
