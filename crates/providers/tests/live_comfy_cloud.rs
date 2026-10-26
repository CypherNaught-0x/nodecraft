//! Disabled free smoke: reads Comfy Cloud's node list with a real key. It runs no workflow, so it
//! spends no credits.
#![cfg(all(feature = "http", feature = "comfy-cloud", not(target_arch = "wasm32")))]
use nodecraft_providers::{ReqwestHttp, comfy_cloud::ComfyCloud, secrets::Secret};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
#[ignore = "live request; NODECRAFT_LIVE_COMFYUI_CLOUD=1 and a key required"]
async fn live_comfy_cloud_node_list() {
    if std::env::var("NODECRAFT_LIVE_COMFYUI_CLOUD").as_deref() != Ok("1") && std::env::var("NODECRAFT_LIVE_COMFY_CLOUD").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("COMFY_CLOUD_API_KEY").expect("provide a key without logging it")).unwrap();
    let cloud = ComfyCloud::new(key, Arc::new(ReqwestHttp::new().unwrap()), Arc::new(|| 0));
    let catalog = cloud.catalog().await.unwrap();
    assert!(catalog.get("SaveImage").is_some(), "Comfy Cloud offers the core nodes");
}
