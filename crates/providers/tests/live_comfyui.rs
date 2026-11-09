//! Opt-in live smoke test against a running ComfyUI server; never runs in CI:
//! `NODECRAFT_LIVE_COMFYUI=1 [NODECRAFT_COMFYUI_URL=http://127.0.0.1:8188] cargo test -p nodecraft-providers --test live_comfyui`.
//! The catalog smoke only reads metadata. Ignored tests below require separate explicit opt-ins:
//! one runs stock image operations without a model, and one queues Hunyuan3D generation.

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

/// Exercise the workflow-card upload, value override, selected-output, queue/history and
/// download path against a real server without a model, custom node or paid API.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires NODECRAFT_LIVE_COMFYUI_WORKFLOW=1; uploads an original image and runs stock image operations"]
async fn live_comfyui_model_free_workflow_card() {
    use nodecraft_providers::{JobInput, JobProgress, Output, Provider, ProviderJob, Submission};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    assert_eq!(std::env::var("NODECRAFT_LIVE_COMFYUI_WORKFLOW").ok().as_deref(), Some("1"), "explicit opt-in required");
    let url = std::env::var("NODECRAFT_COMFYUI_URL").unwrap_or_else(|_| "http://127.0.0.1:8188".into());
    let provider = ComfyProvider::new(&url, None, "live-workflow-card", Arc::new(ReqwestHttp::new().unwrap()), Arc::new(|| 0)).unwrap();
    let limits = nodecraft_media::Limits::default();
    let image = nodecraft_media::encode_png(16, 16, &[40, 80, 120, 255].repeat(16 * 16), &limits).unwrap();
    let workflow = serde_json::json!({
        "1": {"class_type": "LoadImage", "inputs": {"image": "nodecraft-input.png"}},
        "2": {"class_type": "ImageInvert", "inputs": {"image": ["1", 0]}},
        "3": {"class_type": "SaveImage", "inputs": {"images": ["2", 0], "filename_prefix": "unused-before-override"}}
    });
    let job = ProviderJob {
        model: "workflow".into(),
        params: serde_json::Map::new(),
        inputs: vec![
            JobInput::Text { role: "comfy.workflow".into(), text: workflow.to_string() },
            JobInput::Text { role: "comfy.values".into(), text: serde_json::json!({"3.filename_prefix": "NodeCraft-live-contract"}).to_string() },
            JobInput::Text { role: "comfy.outputs".into(), text: serde_json::json!(["3"]).to_string() },
            JobInput::Media { role: "1.image".into(), mime: "image/png".into(), bytes: Arc::from(image) },
        ],
        idempotency_key: format!("live-workflow-card:{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()),
    };
    let started = Instant::now();
    let Submission::Queued(handle) = provider.submit(&job).await.unwrap() else { panic!("queued") };
    loop {
        match provider.poll(&handle).await.unwrap() {
            JobProgress::Done(done) => {
                assert_eq!(done.outputs.len(), 1, "only the selected saver returns an image");
                let Output::Bytes { mime, bytes } = done.outputs.first().unwrap() else { panic!("image bytes") };
                assert_eq!(mime, "image/png");
                let (meta, image) = nodecraft_media::decode_image(bytes, &limits).unwrap();
                assert_eq!((meta.width, meta.height), (16, 16));
                for pixel in image.to_rgba8().pixels() {
                    for (actual, expected) in pixel.0.into_iter().zip([215u8, 175, 135, 255]) {
                        assert!(actual.abs_diff(expected) <= 1, "the real image operation inverted the uploaded fixture");
                    }
                }
                eprintln!("live model-free workflow card returned one verified 16x16 image in {:?}", started.elapsed());
                break;
            }
            JobProgress::Failed(error) => panic!("live model-free workflow failed: {error}"),
            _ => {
                if started.elapsed() >= Duration::from_secs(120) {
                    let _ = provider.cancel(&handle).await;
                    panic!("live model-free workflow timed out; cancellation requested");
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
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
    assert!(image.len() <= 16 * 1024 * 1024, "the live test input limit is 16 MiB");
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
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}
