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

/// Actually queues a single image-to-3D job, unlike the catalog-only smoke above. Deliberately
/// requires both an explicit opt-in and a caller-supplied public-safe image. Never run in CI.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires live ComfyUI/model and NODECRAFT_LIVE_COMFYUI_3D=1; queues one generation"]
async fn comfy3d24_live_shape_output() {
    use nodecraft_providers::{JobInput, JobProgress, Output, Provider, ProviderJob, Submission};
    use std::io::Read as _;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    assert_eq!(std::env::var("NODECRAFT_LIVE_COMFYUI_3D").ok().as_deref(), Some("1"), "explicit opt-in required");
    let input = std::env::var_os("NODECRAFT_COMFYUI_3D_IMAGE").expect("set a public-safe image path");
    let mut image = Vec::new();
    std::fs::File::open(input).unwrap().take(16 * 1024 * 1024 + 1).read_to_end(&mut image).unwrap();
    assert!(image.len() <= 16 * 1024 * 1024, "the live test input limit is16MiB");
    let mime = nodecraft_media::sniff(&image).expect("a recognized image").mime;
    let model = std::env::var("NODECRAFT_COMFYUI_3D_MODEL").unwrap_or_else(|_| "hunyuan3d-2.1".into());
    assert!(matches!(model.as_str(), "hunyuan3d-2.0" | "hunyuan3d-2.1"));
    let url = std::env::var("NODECRAFT_COMFYUI_URL").unwrap_or_else(|_| "http://127.0.0.1:8188".into());
    let provider = ComfyProvider::new(&url, None, "live-comfy3d24", Arc::new(ReqwestHttp::new().unwrap()), Arc::new(|| 0)).unwrap();
    let job = ProviderJob {
        model,
        params: serde_json::json!({"seed":24,"steps":30}).as_object().unwrap().clone(),
        inputs: vec![JobInput::Media { role: "image".into(), mime: mime.into(), bytes: Arc::from(image) }],
        idempotency_key: format!("live-comfy3d24:{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()),
    };
    let Submission::Queued(handle) = provider.submit(&job).await.unwrap() else { panic!("queued") };
    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    loop {
        match provider.poll(&handle).await.unwrap() {
            JobProgress::Done(done) => {
                assert!(!done.outputs.is_empty());
                for output in done.outputs {
                    let Output::Bytes { mime, bytes } = output else { panic!("mesh bytes") };
                    assert!(nodecraft_media::MeshFormat::from_mime(&mime).is_some());
                    assert!(nodecraft_media::decode_mesh(&bytes, &Default::default()).unwrap().triangle_count() > 0);
                }
                break;
            }
            JobProgress::Failed(error) => panic!("live mesh generation failed: {error}"),
            _ => {
                if Instant::now() >= deadline {
                    let _ = provider.cancel(&handle).await;
                    panic!("live mesh generation timed out; cancellation requested");
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}
