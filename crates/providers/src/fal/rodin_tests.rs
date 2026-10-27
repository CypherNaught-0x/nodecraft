//! Rodin 2.5's public schemas through the saved fal queue; authored, offline fixtures only.
use super::tests::{handle, job, png, receipt};
use super::*;
use futures::executor::block_on;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use serde_json::{Value, json};
use std::{collections::VecDeque, io::Cursor, sync::Mutex};

const KEY: &str = "rodin41-fixture-not-a-key";
const MODELS: [&str; 4] = [RODIN_TEXT, RODIN_IMAGES, RODIN_FAST_TEXT, RODIN_FAST_IMAGES];
const TRIANGLE: &[u8] = include_bytes!("fixtures/triangle.glb");

#[derive(Default)]
struct Wire {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Result<HttpResponse, ProviderError>>>,
}
impl Wire {
    fn new(replies: Vec<HttpResponse>) -> Arc<Self> {
        Arc::new(Self { replies: Mutex::new(replies.into_iter().map(Ok).collect()), ..Default::default() })
    }
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirects refused")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { reply })
    }
}
fn reply(value: Value) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&value).unwrap() }
}
fn fal(wire: &Arc<Wire>) -> Fal {
    Fal::new(Secret::new(KEY).unwrap(), wire.clone())
}
fn images_mode(model: &str) -> bool {
    matches!(model, RODIN_IMAGES | RODIN_FAST_IMAGES)
}
fn media(role: &str, mime: &str, bytes: Vec<u8>) -> crate::JobInput {
    crate::JobInput::Media { role: role.into(), mime: mime.into(), bytes: bytes.into() }
}
fn rodin(model: &str) -> ProviderJob {
    let mut j = job(model);
    if images_mode(model) {
        j.inputs.push(media("images", "image/png", png(2, 2)));
    }
    j
}
fn picture(format: ImageFormat, color: u8) -> Vec<u8> {
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, Rgb([color, 70, 120])));
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, format).unwrap();
    out.into_inner()
}

#[test]
fn rodin41_four_routes_pin_the_priced_tier_mesh_material_and_safety_contract() {
    for model in MODELS {
        let wire = Wire::new(vec![reply(receipt(model))]);
        let provider = fal(&wire);
        let Submission::Queued(h) = block_on(provider.submit(&rodin(model))).unwrap() else { panic!() };
        assert_eq!(h, handle(model));
        let requests = wire.requests.lock().unwrap();
        let request = &requests[0];
        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(request.url, format!("{ORIGIN}/{model}"));
        assert!(request.headers.iter().any(|(k, v)| k == "Authorization" && v == &format!("Key {KEY}")));
        assert!(request.headers.iter().any(|(k, v)| k == "X-Fal-No-Retry" && v == "1"));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let fast = matches!(model, RODIN_FAST_TEXT | RODIN_FAST_IMAGES);
        assert_eq!(body["tier"], if fast { "Gen-2.5-Extreme-Low" } else { "Gen-2.5-High" });
        assert_eq!(body["material"], if fast { "Shaded" } else { "PBR" });
        assert_eq!(body["quality_mesh_option"], "Auto");
        assert_eq!(body["geometry_file_format"], "glb");
        assert_eq!(body["enable_safety_checker"], true);
        assert_eq!(body["prompt"], "A blue circle");
        for name in ["enable_creative_mode", "hd_texture", "texture_delight", "is_micro", "TAPose"] {
            assert_eq!(body[name], false);
        }
        for name in ["seed", "addons", "bbox_condition", "texture_mode", "image_url"] {
            assert!(body.get(name).is_none());
        }
        for name in ["image_urls", "use_original_alpha", "preview_render"] {
            assert_eq!(body.get(name).is_some(), images_mode(model));
        }
        if images_mode(model) {
            assert_eq!(body["use_original_alpha"], false);
            assert_eq!(body["preview_render"], false);
        }
        assert!(!provider.retry_ambiguous_submit());
    }
}

#[test]
fn rodin41_fixed_seed_optional_prompt_and_one_to_five_ordered_references_encode() {
    for model in MODELS {
        for seed in [0, 65535] {
            let mut j = rodin(model);
            j.params.insert("seed".into(), json!(seed));
            let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["seed"], seed);
        }
    }
    for model in [RODIN_IMAGES, RODIN_FAST_IMAGES] {
        for prompt in [None, Some(""), Some("A glass bird")] {
            let mut j = job(model);
            j.inputs.clear();
            j.params.insert("use_original_alpha".into(), json!(true));
            if let Some(text) = prompt {
                j.inputs.push(crate::JobInput::Text { role: "prompt".into(), text: text.into() });
            }
            let mut expected = Vec::new();
            for index in 0u8..5 {
                let (mime, format) = match index % 3 {
                    0 => ("image/png", ImageFormat::Png),
                    1 => ("image/jpeg", ImageFormat::Jpeg),
                    _ => ("image/webp", ImageFormat::WebP),
                };
                let bytes = picture(format, index * 30);
                expected.push(format!("data:{mime};base64,{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)));
                j.inputs.push(media("images", mime, bytes));
                let (record, body) = request::prepare(&j, Limits::default()).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(record.requested, 1);
                assert_eq!(body["image_urls"], json!(expected));
                assert_eq!(body["use_original_alpha"], true);
                assert_eq!(body.get("prompt").and_then(Value::as_str), prompt);
            }
        }
    }
}

#[test]
fn rodin41_unicode_prompt_limit_is_characters_in_each_schema() {
    for model in MODELS {
        let mut j = rodin(model);
        j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: "🦋".repeat(1024) };
        assert!(request::prepare(&j, Limits::default()).is_ok());
        j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: "🦋".repeat(1025) };
        assert!(request::prepare(&j, Limits::default()).is_err());
    }
}

#[test]
fn rodin41_bad_roles_types_paid_extras_and_reference_payloads_never_send_http() {
    let mut jobs = Vec::new();
    for model in MODELS {
        for (name, value) in [
            ("seed", json!(-1)),
            ("seed", json!(65536)),
            ("seed", json!(1.5)),
            ("seed", json!("random")),
            ("seed", Value::Null),
            ("tier", json!("Gen-2.5-Extreme-High")),
            ("addons", json!(["HighPack"])),
            ("quality_mesh_option", json!("2M Triangle")),
            ("geometry_file_format", json!("obj")),
            ("material", json!("All")),
            ("hd_texture", json!(true)),
            ("is_micro", json!(true)),
            ("texture_mode", json!("high")),
            ("preview_render", json!(true)),
            ("bbox_condition", json!({"x":1})),
            ("enable_safety_checker", json!(false)),
            ("prompt", json!("a parameter cannot replace a named text input")),
        ] {
            let mut j = rodin(model);
            j.params.insert(name.into(), value);
            jobs.push(j);
        }
        let mut j = rodin(model);
        j.inputs.push(j.inputs[0].clone());
        jobs.push(j);
        let mut j = rodin(model);
        j.inputs.push(media("image", "image/png", png(2, 2)));
        jobs.push(j);
        let mut j = rodin(model);
        j.inputs.push(crate::JobInput::Text { role: "negative_prompt".into(), text: "bad".into() });
        jobs.push(j);
    }
    for model in [RODIN_TEXT, RODIN_FAST_TEXT] {
        let mut missing = job(model);
        missing.inputs.clear();
        jobs.push(missing);
        let mut blank = job(model);
        blank.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: " \n\t".into() };
        jobs.push(blank);
        let mut alpha = job(model);
        alpha.params.insert("use_original_alpha".into(), json!(true));
        jobs.push(alpha);
    }
    for model in [RODIN_IMAGES, RODIN_FAST_IMAGES] {
        jobs.push(job(model));
        let mut too_many = job(model);
        too_many.inputs.extend((0..6).map(|_| media("images", "image/png", png(2, 2))));
        jobs.push(too_many);
        for alpha in [json!("true"), json!(1), Value::Null] {
            let mut j = rodin(model);
            j.params.insert("use_original_alpha".into(), alpha);
            jobs.push(j);
        }
        for (mime, bytes) in
            [("image/png", b"bad image".to_vec()), ("image/jpeg", png(2, 2)), ("image/png", png(1025, 1)), ("image/png", vec![0; 1024 * 1024 + 1])]
        {
            let mut j = job(model);
            j.inputs.push(media("images", mime, bytes));
            jobs.push(j);
        }
    }
    let wire = Arc::new(Wire::default());
    for j in jobs {
        let context = crate::SubmitContext::new();
        assert!(block_on(fal(&wire).submit_with_delivery(&j, &context)).is_err(), "invalid {:?}", j.model);
        assert!(context.finish_error().is_some(), "local refusal must prove no paid submission");
    }
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[test]
fn rodin41_whole_escaped_json_and_all_data_uris_are_bounded_before_codec_work() {
    let mut j = job(RODIN_IMAGES);
    j.inputs.push(media("images", "image/png", b"not a codec payload".to_vec()));
    let error = request::prepare(&j, Limits { request_bytes: 1, ..Limits::default() }).err().unwrap();
    assert!(error.message.contains("escaped request"));
    let mut j = rodin(RODIN_IMAGES);
    j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: "\"\\\n".repeat(100) };
    j.inputs.extend((0..4).map(|_| media("images", "image/png", png(2, 2))));
    let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
    assert!(request::prepare(&j, Limits { request_bytes: bytes.len(), ..Limits::default() }).is_ok());
    assert!(request::prepare(&j, Limits { request_bytes: bytes.len() - 1, ..Limits::default() }).is_err());
    assert!(request::prepare(&j, Limits { text_bytes: 1, ..Limits::default() }).is_err());
    assert!(request::prepare(&j, Limits { input_bytes: 1, ..Limits::default() }).is_err());
}

#[test]
fn rodin41_saved_tasks_use_owner_app_routes_and_download_only_the_canonical_glb() {
    for model in MODELS {
        let wire = Wire::new(vec![
            reply(receipt(model)),
            reply(json!({"status":"IN_QUEUE","queue_position":1})),
            reply(json!({"status":"IN_PROGRESS"})),
            reply(json!({"status":"COMPLETED"})),
            reply(
                json!({"model_mesh":{"url":"https://v3.fal.media/rodin.glb","content_type":"application/octet-stream","file_size":TRIANGLE.len()},"model_meshes":[{"url":"https://unused.example/extra.obj"}],"textures":[{"url":"https://unused.example/texture.png"}]}),
            ),
            HttpResponse { status: 200, headers: vec![], body: TRIANGLE.to_vec() },
            reply(json!({"status":"OK"})),
        ]);
        let provider = fal(&wire);
        let Submission::Queued(h) = block_on(provider.submit(&rodin(model))).unwrap() else { panic!() };
        let saved: JobHandle = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        let root = format!("{ORIGIN}/fal-ai/hyper3d/requests/r_1");
        assert_eq!(saved.data["response_url"], root);
        assert!(matches!(block_on(provider.poll(&saved)).unwrap(), JobProgress::Queued { position: Some(1) }));
        assert!(matches!(block_on(provider.poll(&saved)).unwrap(), JobProgress::Running { .. }));
        let JobProgress::Done(done) = block_on(provider.poll(&saved)).unwrap() else { panic!() };
        assert_eq!(done.outputs, vec![Output::Bytes { mime: "model/gltf-binary".into(), bytes: TRIANGLE.into() }]);
        assert!(done.usage.cost.is_none(), "billable units are not a dollar invoice");
        let mesh = nodecraft_media::decode_mesh(TRIANGLE, &Default::default()).unwrap();
        assert_eq!((mesh.positions.len(), mesh.indices.len()), (3, 3));
        block_on(provider.cancel(&saved)).unwrap();
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 7, "sidecars must never be fetched");
        assert!(requests[5].headers.is_empty(), "storage must receive no credential");
        assert_eq!(requests[6].method, HttpMethod::Put);
        assert_eq!(requests[6].url, format!("{root}/cancel"));
        let mut bad = saved;
        bad.data["status_url"] = json!(format!("{ORIGIN}/{model}/requests/r_1/status"));
        assert!(Record::read(&bad, &Secret::new(KEY).unwrap()).is_err(), "full-alias routes must not replace canonical owner/app routes");
    }
}

fn large_glb() -> Vec<u8> {
    let json_len = usize::try_from(u32::from_le_bytes(TRIANGLE[12..16].try_into().unwrap())).unwrap();
    let mut value: Value = serde_json::from_slice(&TRIANGLE[20..20 + json_len]).unwrap();
    let mut bin = TRIANGLE[20 + json_len + 8..].to_vec();
    bin.resize(17 * 1024 * 1024, 0);
    value["buffers"][0]["byteLength"] = json!(bin.len());
    let mut text = serde_json::to_vec(&value).unwrap();
    while !text.len().is_multiple_of(4) {
        text.push(b' ');
    }
    let len = 20 + text.len() + 8 + bin.len();
    let mut glb = b"glTF".to_vec();
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&u32::try_from(len).unwrap().to_le_bytes());
    glb.extend_from_slice(&u32::try_from(text.len()).unwrap().to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&text);
    glb.extend_from_slice(&u32::try_from(bin.len()).unwrap().to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&bin);
    glb
}

#[test]
fn rodin41_large_packed_glb_resumes_under_scoped_quotas_and_keeps_legacy_limits() {
    for model in MODELS {
        let glb = large_glb();
        assert!(glb.len() > Limits::HARD.output_bytes);
        let mesh = nodecraft_media::decode_mesh(&glb, &Default::default()).unwrap();
        assert_eq!((mesh.positions.len(), mesh.indices.len()), (3, 3));
        let wire = Wire::new(vec![
            reply(json!({"status":"COMPLETED"})),
            reply(json!({"model_mesh":{"url":"https://fal.media/large.glb","file_size":glb.len()}})),
            HttpResponse { status: 200, headers: vec![], body: glb },
        ]);
        assert!(matches!(block_on(fal(&wire).poll(&handle(model))).unwrap(), JobProgress::Done(_)));
        let limits = Limits::default().for_model(Model::read(model).unwrap());
        assert_eq!(limits.output_bytes, 256 * 1024 * 1024);
        assert_eq!(limits.total_output_bytes, limits.output_bytes);
        assert_eq!(limits.request_bytes, if images_mode(model) { 8 * 1024 * 1024 } else { 4 * 1024 * 1024 });
    }
    assert_eq!(Limits::default().for_model(Model::Mesh).output_bytes, Limits::HARD.output_bytes);
    assert_eq!(Limits::default().for_model(Model::Image).request_bytes, Limits::HARD.request_bytes);
}

#[test]
fn rodin41_bad_output_origins_envelopes_sizes_and_actual_mesh_bytes_are_refused() {
    for model in MODELS {
        let record = Record::read(&handle(model), &Secret::new(KEY).unwrap()).unwrap().0;
        let limits = Limits::default().for_model(record.model);
        for value in [
            json!({"model_mesh":{"url":"https://evil.example/mesh"}}),
            json!({"model_mesh":{"url":"https://fal.media/mesh","file_size":268435457u64}}),
            json!({"model_mesh":{"url":"https://fal.media/mesh","content_type":"application/zip"}}),
            json!({"model_mesh":{"url":"https://fal.media/mesh"},"model_glb":{"url":"https://fal.media/other"}}),
            json!({"model_mesh":{"url":"https://fal.media/mesh"},"images":[]}),
            json!({"model_meshes":[{"url":"https://fal.media/sidecar"}]}),
        ] {
            assert!(response::outputs(&serde_json::to_vec(&value).unwrap(), record, limits, &Secret::new(KEY).unwrap()).is_err());
        }
        assert!(media::validate(b"bad glb", record, limits, &mut 0).is_err());
        let mut corrupt = TRIANGLE.to_vec();
        corrupt[8..12].copy_from_slice(&1u32.to_le_bytes());
        assert!(media::validate(&corrupt, record, limits, &mut 0).is_err());
    }
}

#[test]
fn rodin41_remote_failures_lost_replies_and_ambiguous_receipts_keep_billing_uncertain() {
    for (status, kind) in [
        (400, ErrorKind::UnsupportedInput),
        (401, ErrorKind::Auth),
        (402, ErrorKind::Budget),
        (403, ErrorKind::Auth),
        (404, ErrorKind::NotFound),
        (422, ErrorKind::UnsupportedInput),
        (429, ErrorKind::RateLimited),
        (500, ErrorKind::Server),
        (503, ErrorKind::Busy),
        (504, ErrorKind::Timeout),
    ] {
        let wire = Wire::new(vec![HttpResponse { status, headers: vec![("Retry-After".into(), "2".into())], body: KEY.as_bytes().to_vec() }]);
        let context = crate::SubmitContext::new();
        let error = block_on(fal(&wire).submit_with_delivery(&rodin(RODIN_TEXT), &context)).unwrap_err();
        assert_eq!(error.kind, kind);
        assert!(!error.message.contains(KEY));
        assert!(context.finish_error().is_none());
        if status == 429 {
            assert_eq!(error.retry_after_ms, Some(2000));
        }
    }
    for model in MODELS {
        for response in [
            Err(ProviderError::new(ErrorKind::Network, KEY)),
            Ok(HttpResponse { status: 200, headers: vec![], body: br#"{"request_id":"r_1","request_id":"r_2","status":"IN_QUEUE"}"#.to_vec() }),
        ] {
            let wire = Arc::new(Wire { replies: Mutex::new(vec![response].into()), ..Default::default() });
            let context = crate::SubmitContext::new();
            let error = block_on(fal(&wire).submit_with_delivery(&rodin(model), &context)).unwrap_err();
            assert!(!error.message.contains(KEY));
            assert!(context.finish_error().is_none());
            assert_eq!(wire.requests.lock().unwrap().len(), 1);
            assert!(!fal(&wire).retry_ambiguous_submit());
        }
    }
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[test]
#[ignore = "paid live smoke: NODECRAFT_LIVE_RODIN=1 + NODECRAFT_LIVE_RODIN_ACKNOWLEDGE_COST=1 + FAL_KEY; one fast text GLB costs $0.10"]
fn rodin41_live_smoke() {
    if std::env::var("NODECRAFT_LIVE_RODIN").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_RODIN_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("FAL_KEY").unwrap()).unwrap();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let provider = Fal::new(key, Arc::new(crate::http::ReqwestHttp::new().unwrap()));
        let Submission::Queued(handle) = provider.submit(&rodin(RODIN_FAST_TEXT)).await.unwrap() else { panic!() };
        for _ in 0..180 {
            match provider.poll(&handle).await.unwrap() {
                JobProgress::Done(done) => {
                    assert_eq!(done.outputs.len(), 1);
                    let Output::Bytes { bytes, .. } = &done.outputs[0] else { panic!("packed GLB expected") };
                    nodecraft_media::decode_mesh(bytes, &Default::default()).unwrap();
                    return;
                }
                JobProgress::Failed(error) => panic!("{}", error.message),
                _ => tokio::time::sleep(std::time::Duration::from_secs(5)).await,
            }
        }
        let _ = provider.cancel(&handle).await;
        panic!("Rodin live task timed out");
    });
}
