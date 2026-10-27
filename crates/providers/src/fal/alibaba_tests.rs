//! Alibaba schemas, bounds and saved fal tasks use authored offline fixtures only.
use super::tests::{handle, job, png, receipt};
use super::*;
use futures::executor::block_on;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use serde_json::{Value, json};
use std::{collections::VecDeque, io::Cursor, sync::Mutex};

const KEY: &str = "alibaba32-fixture-not-a-key";
const MODELS: [&str; 4] = [QWEN_IMAGE, QWEN_EDIT, WAN3_VIDEO, WAN3_I2V];

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
fn rgb(width: u32, height: u32, format: ImageFormat, color: u8) -> Vec<u8> {
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(width, height, Rgb([color, 60, 120])));
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, format).unwrap();
    out.into_inner()
}
fn media(role: &str, mime: &str, bytes: Vec<u8>) -> crate::JobInput {
    crate::JobInput::Media { role: role.into(), mime: mime.into(), bytes: bytes.into() }
}
fn alibaba(model: &str) -> ProviderJob {
    let mut j = job(model);
    if model == QWEN_EDIT {
        j.inputs.push(media("images", "image/png", rgb(384, 384, ImageFormat::Png, 20)));
    }
    if model == WAN3_I2V {
        j.inputs.push(media("first_frame", "image/png", png(2, 2)));
    }
    j
}

#[test]
fn alibaba32_routes_defaults_and_fixed_safety_use_each_models_actual_schema() {
    for model in MODELS {
        let wire = Wire::new(vec![reply(receipt(model))]);
        let provider = fal(&wire);
        let Submission::Queued(h) = block_on(provider.submit(&alibaba(model))).unwrap() else { panic!() };
        assert_eq!(h, handle(model));
        let requests = wire.requests.lock().unwrap();
        let request = &requests[0];
        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(request.url, format!("{ORIGIN}/{model}"));
        assert!(request.headers.iter().any(|(k, v)| k == "Authorization" && v == &format!("Key {KEY}")));
        assert!(request.headers.iter().any(|(k, v)| k == "X-Fal-No-Retry" && v == "1"));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["prompt"], "A blue circle");
        assert_eq!(body["enable_safety_checker"], true);
        assert_eq!(body["enable_prompt_expansion"], true);
        assert!(body.get("seed").is_none(), "omitting a fixed seed must not invent one");
        if matches!(model, QWEN_IMAGE | QWEN_EDIT) {
            assert_eq!(body["image_size"], json!({"width":1024,"height":1024}));
            assert_eq!(body["num_images"], 1);
            assert_eq!(body["negative_prompt"], "");
            assert_eq!(body["output_format"], "png");
            assert_eq!(body["sync_mode"], false);
            assert!(body.get("resolution").is_none());
            assert!(body.get("duration").is_none());
            assert!(body.get("audio").is_none());
            assert_eq!(body.get("image_urls").is_some(), model == QWEN_EDIT);
        } else {
            assert_eq!(body["duration"], 5);
            assert_eq!(body["resolution"], "720p");
            assert_eq!(body["aspect_ratio"], "16:9");
            assert_eq!(body["audio"], true);
            assert_eq!(body["enable_thinking"], false);
            for key in ["num_images", "image_size", "output_format", "sync_mode", "negative_prompt", "image_url"] {
                assert!(body.get(key).is_none());
            }
            assert_eq!(body.get("start_image_url").is_some(), model == WAN3_I2V);
            assert!(body.get("end_image_url").is_none());
        }
        assert!(!provider.retry_ambiguous_submit());
    }
}

#[test]
fn alibaba32_qwen_explicit_priced_sizes_counts_formats_and_reference_order_encode() {
    for (preset, side) in [("1k-square", 1024), ("2k-square", 2048)] {
        for count in 1..=6 {
            for format in ["png", "jpeg", "webp"] {
                let mut j = alibaba(QWEN_IMAGE);
                j.params = serde_json::from_value(json!({"image_preset":preset,"num_images":count,"output_format":format,"enable_prompt_expansion":false,"negative_prompt":"blur","seed":2147483647})).unwrap();
                let (record, bytes) = request::prepare(&j, Limits::default()).unwrap();
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(record.requested, count);
                assert_eq!(body["image_size"], json!({"width":side,"height":side}));
                assert_eq!(body["num_images"], count);
                assert_eq!(body["output_format"], format);
                assert_eq!(body["enable_prompt_expansion"], false);
                assert_eq!(body["negative_prompt"], "blur");
                assert_eq!(body["seed"], 2147483647u64);
            }
        }
    }
    let mut j = job(QWEN_EDIT);
    let formats = [("image/png", ImageFormat::Png), ("image/jpeg", ImageFormat::Jpeg), ("image/webp", ImageFormat::WebP)];
    let mut expected = Vec::new();
    for (index, (mime, format)) in formats.into_iter().enumerate() {
        let bytes = rgb(384, 384, format, u8::try_from(index * 30).unwrap());
        expected.push(format!("data:{mime};base64,{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)));
        j.inputs.push(media("images", mime, bytes));
        let (_, body) = request::prepare(&j, Limits::default()).unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["image_urls"], json!(expected));
    }
}

#[test]
fn alibaba32_wan_explicit_duration_resolution_aspect_and_boolean_controls_encode() {
    for duration in 2..=30 {
        for resolution in ["480p", "720p", "1080p"] {
            let mut j = alibaba(WAN3_VIDEO);
            j.params = serde_json::from_value(
                json!({"duration_seconds":duration,"resolution":resolution,"audio":false,"enable_prompt_expansion":false,"enable_thinking":true,"seed":0}),
            )
            .unwrap();
            let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["duration"], duration);
            assert_eq!(body["resolution"], resolution);
            assert_eq!(body["audio"], false);
            assert_eq!(body["enable_prompt_expansion"], false);
            assert_eq!(body["enable_thinking"], true);
            assert_eq!(body["seed"], 0);
        }
    }
    for model in [WAN3_VIDEO, WAN3_I2V] {
        for ratio in ["adaptive", "16:9", "4:3", "1:1", "3:4", "9:16"] {
            let mut j = alibaba(model);
            j.params.insert("aspect_ratio".into(), json!(ratio));
            let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["aspect_ratio"], ratio);
        }
    }
    for prompt in [None, Some("")] {
        let mut j = job(WAN3_I2V);
        j.inputs.clear();
        if let Some(text) = prompt {
            j.inputs.push(crate::JobInput::Text { role: "prompt".into(), text: text.into() });
        }
        let first = png(2, 2);
        let last = png(3, 2);
        // Named frame order, rather than the order of arbitrary caller inputs.
        j.inputs.push(media("last_frame", "image/png", last.clone()));
        j.inputs.push(media("first_frame", "image/png", first.clone()));
        let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body.get("prompt").and_then(Value::as_str), prompt);
        for (key, raw) in [("start_image_url", first), ("end_image_url", last)] {
            assert_eq!(body[key], format!("data:image/png;base64,{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw)));
        }
    }
}

#[test]
fn alibaba32_unicode_prompt_boundaries_fit_scoped_text_limits_without_widening_legacy_models() {
    for (model, count) in [(QWEN_IMAGE, 5000), (WAN3_VIDEO, 20000)] {
        let mut j = job(model);
        j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: "🦋".repeat(count) };
        if model == QWEN_IMAGE {
            j.params.insert("negative_prompt".into(), json!("雪".repeat(500)));
        }
        assert!(request::prepare(&j, Limits::default()).is_ok());
        j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: "🦋".repeat(count + 1) };
        assert!(request::prepare(&j, Limits::default()).is_err());
    }
    assert_eq!(Limits::default().for_model(Model::Image).text_bytes, Limits::HARD.text_bytes);
    assert_eq!(Limits::default().for_model(Model::Wan3Text).text_bytes, 96 * 1024);
    let mut legacy = job(FLUX);
    legacy.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: "x".repeat(20_000) };
    assert!(request::prepare(&legacy, Limits::default()).is_err());
}

#[test]
fn alibaba32_invalid_fields_roles_reference_headers_and_bounds_never_send_paid_http() {
    let mut jobs = vec![job(QWEN_EDIT), job(WAN3_I2V)];
    for (model, name, value) in [
        (QWEN_IMAGE, "num_images", json!(0)),
        (QWEN_IMAGE, "num_images", json!(7)),
        (QWEN_IMAGE, "image_preset", json!("auto")),
        (QWEN_IMAGE, "image_preset", json!("1.5k-square")),
        (QWEN_IMAGE, "image_size", json!({"width":1024,"height":1024})),
        (QWEN_IMAGE, "resolution", json!("2k")),
        (QWEN_IMAGE, "output_format", json!("gif")),
        (QWEN_IMAGE, "negative_prompt", json!("a".repeat(501))),
        (QWEN_IMAGE, "negative_prompt", json!(false)),
        (QWEN_IMAGE, "enable_prompt_expansion", json!("true")),
        (WAN3_VIDEO, "duration_seconds", json!(1)),
        (WAN3_VIDEO, "duration_seconds", json!(31)),
        (WAN3_VIDEO, "duration_seconds", Value::Null),
        (WAN3_VIDEO, "duration_seconds", json!("auto")),
        (WAN3_VIDEO, "resolution", json!("4k")),
        (WAN3_VIDEO, "aspect_ratio", json!("auto")),
        (WAN3_VIDEO, "audio", json!("false")),
        (WAN3_VIDEO, "enable_prompt_expansion", json!(1)),
        (WAN3_VIDEO, "enable_thinking", json!("true")),
        (WAN3_I2V, "end_image_url", json!("https://unused.example/last.png")),
    ] {
        let mut j = alibaba(model);
        j.params.insert(name.into(), value);
        jobs.push(j);
    }
    for model in MODELS {
        for (name, value) in [
            ("seed", json!(-1)),
            ("seed", json!(2147483648u64)),
            ("seed", json!("random")),
            ("sync_mode", json!(true)),
            ("enable_safety_checker", json!(false)),
        ] {
            let mut j = alibaba(model);
            j.params.insert(name.into(), value);
            jobs.push(j);
        }
        let mut j = alibaba(model);
        j.inputs.push(j.inputs[0].clone());
        jobs.push(j);
        let mut j = alibaba(model);
        j.inputs.push(media("unsupported", "image/png", png(2, 2)));
        jobs.push(j);
    }
    for model in [QWEN_IMAGE, QWEN_EDIT, WAN3_VIDEO] {
        let mut j = alibaba(model);
        j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: " ".into() };
        jobs.push(j);
    }
    let mut no_first = job(WAN3_I2V);
    no_first.inputs.push(media("last_frame", "image/png", png(2, 2)));
    jobs.push(no_first);
    let mut duplicate_frame = alibaba(WAN3_I2V);
    duplicate_frame.inputs.push(media("first_frame", "image/png", png(2, 2)));
    jobs.push(duplicate_frame);
    let mut too_many = job(QWEN_EDIT);
    too_many.inputs.extend((0..4).map(|_| media("images", "image/png", rgb(384, 384, ImageFormat::Png, 20))));
    jobs.push(too_many);
    for (mime, bytes) in [
        ("image/png", b"bad".to_vec()),
        ("image/jpeg", rgb(384, 384, ImageFormat::Png, 20)),
        ("image/png", rgb(383, 384, ImageFormat::Png, 20)),
        ("image/png", rgb(384, 383, ImageFormat::Png, 20)),
        ("image/png", rgb(1025, 384, ImageFormat::Png, 20)),
        ("image/png", png(384, 384)),
        ("image/png", nodecraft_media::encode_png(384, 384, &vec![255; 384 * 384 * 4], &Default::default()).unwrap()),
    ] {
        let mut j = job(QWEN_EDIT);
        j.inputs.push(media("images", mime, bytes));
        jobs.push(j);
    }
    let wire = Arc::new(Wire::default());
    for j in jobs {
        let context = crate::SubmitContext::new();
        assert!(block_on(fal(&wire).submit_with_delivery(&j, &context)).is_err(), "invalid {:?}", j.model);
        assert!(context.finish_error().is_some(), "local refusal must prove whole-submit nonacceptance");
    }
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[test]
fn alibaba32_lowered_geometry_and_complete_escaped_request_are_admitted_before_codecs() {
    let mut j = job(QWEN_EDIT);
    j.inputs.push(media("images", "image/png", b"not a codec payload".to_vec()));
    let error = request::prepare(&j, Limits { output_width: 1023, ..Limits::default() }).err().unwrap();
    assert!(error.message.contains("geometry"));
    let error = request::prepare(&j, Limits { request_bytes: 1, ..Limits::default() }).err().unwrap();
    assert!(error.message.contains("escaped request"));
    let j = alibaba(QWEN_EDIT);
    let (_, body) = request::prepare(&j, Limits::default()).unwrap();
    assert!(request::prepare(&j, Limits { request_bytes: body.len(), ..Limits::default() }).is_ok());
    assert!(request::prepare(&j, Limits { request_bytes: body.len() - 1, ..Limits::default() }).is_err());
    let mut j = job(QWEN_IMAGE);
    j.params.insert("num_images".into(), json!(2));
    assert!(request::prepare(&j, Limits { total_pixels: 2 * 1024 * 1024 - 1, ..Limits::default() }).is_err());
    assert!(request::prepare(&j, Limits { output_height: 1023, ..Limits::default() }).is_err());
    assert!(request::prepare(&alibaba(WAN3_I2V), Limits { input_bytes: 1, ..Limits::default() }).is_err());
}

#[test]
fn alibaba32_saved_handles_resume_six_images_or_one_large_video_without_storage_credentials() {
    for model in MODELS {
        let video = matches!(model, WAN3_VIDEO | WAN3_I2V);
        let mut data = if video { include_bytes!("fixtures/blue.mp4").to_vec() } else { png(2, 2) };
        if video {
            let padding = 17 * 1024 * 1024usize;
            data.extend_from_slice(&u32::try_from(padding + 8).unwrap().to_be_bytes());
            data.extend_from_slice(b"free");
            data.resize(data.len() + padding, 0);
        }
        let file = json!({"url":"https://v3.fal.media/files/public/alibaba-output","file_size":data.len()});
        let output = if video { json!({"video":file}) } else { json!({"images":vec![file;6]}) };
        let mut responses = vec![reply(receipt(model)), reply(json!({"status":"IN_PROGRESS"})), reply(json!({"status":"COMPLETED"})), reply(output)];
        responses.extend((0..if video { 1 } else { 6 }).map(|_| HttpResponse { status: 200, headers: vec![], body: data.clone() }));
        responses.push(HttpResponse { status: 202, headers: vec![], body: serde_json::to_vec(&json!({"status":"CANCELLATION_REQUESTED"})).unwrap() });
        let wire = Wire::new(responses);
        let provider = fal(&wire);
        let mut j = alibaba(model);
        if !video {
            j.params.insert("num_images".into(), json!(6));
        }
        let Submission::Queued(h) = block_on(provider.submit(&j)).unwrap() else { panic!() };
        let saved: JobHandle = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        assert!(matches!(block_on(provider.poll(&saved)).unwrap(), JobProgress::Running { .. }));
        let JobProgress::Done(done) = block_on(provider.poll(&saved)).unwrap() else { panic!() };
        assert_eq!(done.outputs.len(), if video { 1 } else { 6 });
        assert!(done.usage.cost.is_none(), "fal units are not a USD invoice");
        block_on(provider.cancel(&saved)).unwrap();
        let requests = wire.requests.lock().unwrap();
        for request in requests.iter().filter(|r| r.url.starts_with("https://v3.fal.media")) {
            assert!(request.headers.is_empty());
        }
        assert_eq!(requests.last().unwrap().method, HttpMethod::Put);
        let mut bad = saved;
        bad.data["status_url"] = json!("https://queue.fal.run/alibaba/other/requests/r_1/status");
        assert!(Record::read(&bad, &Secret::new(KEY).unwrap()).is_err());
    }
    for model in [Model::Image, Model::QwenImage, Model::QwenEdit, Model::Wan3Text, Model::Wan3Frames] {
        let limits = Limits::default().for_model(model);
        assert_eq!(limits.output_bytes, if matches!(model, Model::Wan3Text | Model::Wan3Frames) { 256 * 1024 * 1024 } else { 16 * 1024 * 1024 });
        assert_eq!(
            limits.total_output_bytes,
            if matches!(model, Model::QwenImage | Model::QwenEdit) {
                96 * 1024 * 1024
            } else if matches!(model, Model::Wan3Text | Model::Wan3Frames) {
                256 * 1024 * 1024
            } else {
                Limits::HARD.total_output_bytes
            }
        );
        assert_eq!(limits.request_bytes, if model == Model::QwenEdit { 8 * 1024 * 1024 } else { 4 * 1024 * 1024 });
    }
}

#[test]
fn alibaba32_remote_failures_and_lost_submissions_never_invent_no_charge_proof() {
    for status in [400, 401, 402, 403, 422, 429, 500, 503] {
        let wire = Wire::new(vec![HttpResponse { status, headers: vec![("Retry-After".into(), "2".into())], body: KEY.as_bytes().to_vec() }]);
        let context = crate::SubmitContext::new();
        let error = block_on(fal(&wire).submit_with_delivery(&alibaba(QWEN_IMAGE), &context)).unwrap_err();
        assert!(!error.message.contains(KEY));
        assert!(context.finish_error().is_none());
        if status == 429 {
            assert_eq!(error.retry_after_ms, Some(2000));
        }
    }
    let wire = Arc::new(Wire { replies: Mutex::new(vec![Err(ProviderError::new(ErrorKind::Network, KEY))].into()), ..Default::default() });
    let context = crate::SubmitContext::new();
    let error = block_on(fal(&wire).submit_with_delivery(&alibaba(WAN3_VIDEO), &context)).unwrap_err();
    assert!(!error.message.contains(KEY));
    assert!(context.finish_error().is_none());
    assert!(!fal(&wire).retry_ambiguous_submit());
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[test]
#[ignore = "paid live smoke: NODECRAFT_LIVE_ALIBABA=1 + NODECRAFT_LIVE_ALIBABA_ACKNOWLEDGE_COST=1 + FAL_KEY; one 1K Qwen image costs $0.04"]
fn alibaba32_live_smoke() {
    if std::env::var("NODECRAFT_LIVE_ALIBABA").as_deref() != Ok("1") || std::env::var("NODECRAFT_LIVE_ALIBABA_ACKNOWLEDGE_COST").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("FAL_KEY").unwrap()).unwrap();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let provider = Fal::new(key, Arc::new(crate::http::ReqwestHttp::new().unwrap()));
        let Submission::Queued(handle) = provider.submit(&alibaba(QWEN_IMAGE)).await.unwrap() else { panic!() };
        for _ in 0..180 {
            match provider.poll(&handle).await.unwrap() {
                JobProgress::Done(done) => {
                    assert_eq!(done.outputs.len(), 1);
                    return;
                }
                JobProgress::Failed(error) => panic!("{}", error.message),
                _ => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
            }
        }
        let _ = provider.cancel(&handle).await;
        panic!("live Qwen task timed out");
    });
}
