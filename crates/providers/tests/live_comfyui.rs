//! Opt-in live smoke test against a running ComfyUI server; never runs in CI:
//! `NODECRAFT_LIVE_COMFYUI=1 [NODECRAFT_COMFYUI_URL=http://127.0.0.1:8188] cargo test -p nodecraft-providers --test live_comfyui`.
//! It only reads the server's node catalog (no prompt is queued, nothing is generated).

#![cfg(all(feature = "http", feature = "comfyui", not(target_arch = "wasm32")))]

use std::sync::Arc;

use nodecraft_providers::ReqwestHttp;
use nodecraft_providers::comfyui::ComfyProvider;

#[tokio::test(flavor = "current_thread")]
async fn live_comfyui_catalog() {
    if std::env::var("NODECRAFT_LIVE_COMFYUI").ok().as_deref() != Some("1") {
        return;
    }
    let url = std::env::var("NODECRAFT_COMFYUI_URL").unwrap_or_else(|_| "http://127.0.0.1:8188".into());
    let provider = ComfyProvider::new(&url, None, "live-test", Arc::new(ReqwestHttp::new().unwrap()), Arc::new(|| 0)).unwrap();
    let catalog = provider.catalog().await.unwrap();
    assert!(catalog.contains("KSampler"), "a stock ComfyUI has KSampler");
}
