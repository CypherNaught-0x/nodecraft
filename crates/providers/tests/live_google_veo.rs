//! Ignored paid smoke; never selected in CI and never run without separate spending approval.
//! One Veo 3.1 Fast clip, 4 s at 720p with sound: about 0.40 USD at Google's listed price.
#![cfg(all(feature = "http", feature = "google-veo", not(target_arch = "wasm32")))]
use nodecraft_providers::google_cloud::veo::{FAST, Veo};
use nodecraft_providers::google_cloud::{compact, system_clock};
use nodecraft_providers::{JobInput, JobProgress, Provider, ProviderJob, ReqwestHttp, Submission};
use std::sync::Arc;
#[tokio::test(flavor = "current_thread")]
#[ignore = "paid Veo request; requires explicit human authorization and both guards"]
async fn live_google_veo() {
    if std::env::var("NODECRAFT_LIVE_GOOGLE_VEO").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_GOOGLE_VEO_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    let path = std::env::var("GOOGLE_VEO_KEY_FILE").expect("the path of an authorized service-account key file");
    let secret = compact(&std::fs::read_to_string(path).expect("the key file can be read")).expect("a service-account key file");
    let p = Veo::new(&secret, Arc::new(ReqwestHttp::new().unwrap()), system_clock()).unwrap();
    let mut job = ProviderJob {
        model: FAST.into(),
        params: Default::default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "A paper boat drifting down a rain gutter, close up.".into() }],
        idempotency_key: "explicit-live-no-dedup-promise".into(),
    };
    job.params.insert("duration_seconds".into(), 4.into());
    job.params.insert("resolution".into(), "720p".into());
    let Submission::Queued(handle) = p.submit(&job).await.unwrap() else { panic!("a long-running operation") };
    for _ in 0..60 {
        match p.poll(&handle).await.unwrap() {
            JobProgress::Done(done) => return assert_eq!(done.outputs.len(), 1),
            JobProgress::Failed(error) => panic!("{}", error.message),
            _ => tokio::time::sleep(std::time::Duration::from_secs(10)).await,
        }
    }
    panic!("Veo didn't finish within ten minutes");
}
