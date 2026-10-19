//! Disabled paid smoke. Execution also requires separate human spending authorization.
#![cfg(all(feature = "http", feature = "seedream", not(target_arch = "wasm32")))]
use nodecraft_providers::{
    JobInput, Provider, ProviderJob, ReqwestHttp, Submission,
    secrets::Secret,
    seedream::{MODEL, Seedream},
};
use std::sync::Arc;
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid request; explicit human authorization and both environment guards required"]
async fn live_seedream() {
    if std::env::var("NODECRAFT_LIVE_SEEDREAM").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_SEEDREAM_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("ARK_API_KEY").expect("provide an authorized key without logging it")).unwrap();
    let p = Seedream::new(key, Arc::new(ReqwestHttp::new().unwrap()));
    let j = ProviderJob {
        model: MODEL.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A small plain blue circle on a white background.".into() }],
        idempotency_key: "explicit-live-no-server-dedup".into(),
    };
    let Submission::Finished(c) = p.submit(&j).await.unwrap() else { panic!() };
    assert_eq!(c.outputs.len(), 1);
}
