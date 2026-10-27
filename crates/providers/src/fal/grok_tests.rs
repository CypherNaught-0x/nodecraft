//! Offline Grok schemas and failure billing through the actual fal queue adapter.
use super::tests::{handle, job, png, receipt};
use super::*;
use futures::executor::block_on;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};
const KEY: &str = "grok31-public-fixture-key";
const MODELS: [&str; 4] = [GROK_IMAGE, GROK_EDIT, GROK_VIDEO, GROK_I2V];
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
fn reference(role: &str) -> crate::JobInput {
    crate::JobInput::Media { role: role.into(), mime: "image/png".into(), bytes: png(2, 2).into() }
}
fn grok(model: &str) -> ProviderJob {
    let mut j = job(model);
    if model == GROK_EDIT {
        j.inputs.extend((0..5).map(|_| reference("images")));
    }
    if model == GROK_I2V {
        j.inputs.push(reference("first_frame"));
    }
    j
}

#[test]
fn grok31_four_routes_use_their_own_schema_without_ignored_settings() {
    for model in MODELS {
        let wire = Wire::new(vec![reply(receipt(model))]);
        let Submission::Queued(h) = block_on(fal(&wire).submit(&grok(model))).unwrap() else { panic!() };
        assert_eq!(h, handle(model));
        let requests = wire.requests.lock().unwrap();
        let request = &requests[0];
        assert_eq!(request.url, format!("{ORIGIN}/{model}"));
        assert!(request.headers.iter().any(|(k, v)| k == "Authorization" && v == &format!("Key {KEY}")));
        assert!(request.headers.iter().any(|(k, v)| k == "X-Fal-No-Retry" && v == "1"));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["prompt"], "A blue circle");
        assert_eq!(body["enable_safety_checker"], true);
        if matches!(model, GROK_IMAGE | GROK_EDIT) {
            assert_eq!(body["resolution"], "1k");
            assert_eq!(body["quality"], "medium");
            assert_eq!(body["num_images"], 1);
            assert_eq!(body["output_format"], "jpeg");
            assert_eq!(body["aspect_ratio"], if model == GROK_EDIT { "auto" } else { "1:1" });
            assert!(body.get("duration").is_none());
            if model == GROK_EDIT {
                assert_eq!(body["image_urls"].as_array().unwrap().len(), 5);
            }
        } else {
            assert_eq!(body["resolution"], "720p");
            assert_eq!(body["duration"], 6);
            assert!(body.get("quality").is_none());
            if model == GROK_I2V {
                assert!(body["image_url"].as_str().unwrap().starts_with("data:image/png;base64,"));
                assert!(body.get("aspect_ratio").is_none());
            } else {
                assert_eq!(body["aspect_ratio"], "16:9");
            }
        }
        assert!(!fal(&wire).retry_ambiguous_submit());
    }
}

#[test]
fn grok31_all_advertised_presets_counts_durations_and_resolutions_encode() {
    for model in [GROK_IMAGE, GROK_EDIT] {
        for preset in ["low-1k", "low-2k", "medium-1k", "medium-2k"] {
            for count in 1..=10 {
                for format in ["jpeg", "png", "webp"] {
                    let mut j = grok(model);
                    j.params = serde_json::from_value(json!({"image_preset":preset,"num_images":count,"output_format":format})).unwrap();
                    let (record, bytes) = request::prepare(&j, Limits::default()).unwrap();
                    let body: Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(record.requested, count);
                    assert_eq!(body["num_images"], count);
                    assert_eq!(body["output_format"], format);
                    assert_eq!(format!("{}-{}", body["quality"].as_str().unwrap(), body["resolution"].as_str().unwrap()), preset);
                }
            }
        }
    }
    for model in [GROK_VIDEO, GROK_I2V] {
        for duration in 1..=15 {
            for resolution in ["480p", "720p", "1080p"] {
                let mut j = grok(model);
                j.params = serde_json::from_value(json!({"duration_seconds":duration,"resolution":resolution})).unwrap();
                let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(body["duration"], duration);
                assert_eq!(body["resolution"], resolution);
            }
        }
    }
}

#[test]
fn grok31_bad_roles_values_references_and_requests_refuse_before_http() {
    let wire = Arc::new(Wire::default());
    let mut jobs = vec![job(GROK_EDIT), job(GROK_I2V)];
    for (model, key, value) in [
        (GROK_IMAGE, "num_images", json!(11)),
        (GROK_IMAGE, "num_images", json!(0)),
        (GROK_IMAGE, "image_preset", json!("auto-2k")),
        (GROK_IMAGE, "image_preset", json!("low-1.5k")),
        (GROK_IMAGE, "seed", json!(1)),
        (GROK_IMAGE, "enable_safety_checker", json!(false)),
        (GROK_VIDEO, "duration_seconds", json!(0)),
        (GROK_VIDEO, "duration_seconds", json!(16)),
        (GROK_VIDEO, "duration_seconds", json!("6")),
        (GROK_VIDEO, "resolution", json!("4k")),
        (GROK_I2V, "aspect_ratio", json!("16:9")),
        (GROK_I2V, "generate_audio", json!(false)),
    ] {
        let mut j = grok(model);
        j.params.insert(key.into(), value);
        jobs.push(j);
    }
    for model in MODELS {
        let mut j = grok(model);
        j.inputs.push(j.inputs[0].clone());
        jobs.push(j);
        let mut j = grok(model);
        j.inputs.push(reference("unrecognized"));
        jobs.push(j);
        let mut j = grok(model);
        j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: " ".into() };
        jobs.push(j);
        let mut j = grok(model);
        j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: "a".repeat(if matches!(model, GROK_IMAGE | GROK_EDIT) { 8001 } else { 4097 }) };
        jobs.push(j);
    }
    for (mime, bytes) in [("image/png", b"bad".to_vec()), ("image/jpeg", png(2, 2)), ("image/png", png(1025, 1))] {
        let mut j = grok(GROK_I2V);
        j.inputs[1] = crate::JobInput::Media { role: "first_frame".into(), mime: mime.into(), bytes: bytes.into() };
        jobs.push(j);
    }
    for j in jobs {
        let context = crate::SubmitContext::new();
        assert!(block_on(fal(&wire).submit_with_delivery(&j, &context)).is_err());
        assert!(context.finish_error().is_some(), "local refusal proves no paid request");
    }
    assert!(wire.requests.lock().unwrap().is_empty());
    let j = grok(GROK_EDIT);
    let (_, body) = request::prepare(&j, Limits::default()).unwrap();
    assert!(request::prepare(&j, Limits { request_bytes: body.len(), ..Limits::default() }).is_ok());
    assert!(request::prepare(&j, Limits { request_bytes: body.len() - 1, ..Limits::default() }).is_err());
    assert!(request::prepare(&j, Limits { input_bytes: 1, ..Limits::default() }).is_err());
}

#[test]
fn grok31_resumes_canonical_handles_and_downloads_ten_images_or_one_video_without_keys() {
    for model in MODELS {
        let video = matches!(model, GROK_VIDEO | GROK_I2V);
        let data = if video { include_bytes!("fixtures/blue.mp4").to_vec() } else { png(2, 2) };
        let file = json!({"url":"https://v3.fal.media/files/public/grok-output","file_size":data.len()});
        let output = if video { json!({"video":file}) } else { json!({"images":vec![file;10]}) };
        let mut replies = vec![reply(receipt(model)), reply(json!({"status":"IN_PROGRESS"})), reply(json!({"status":"COMPLETED"})), reply(output)];
        replies.extend((0..if video { 1 } else { 10 }).map(|_| HttpResponse { status: 200, headers: vec![], body: data.clone() }));
        replies.push(HttpResponse { status: 202, headers: vec![], body: serde_json::to_vec(&json!({"status":"CANCELLATION_REQUESTED"})).unwrap() });
        let wire = Wire::new(replies);
        let f = fal(&wire);
        let mut j = grok(model);
        if !video {
            j.params.insert("num_images".into(), json!(10));
        }
        let Submission::Queued(h) = block_on(f.submit(&j)).unwrap() else { panic!() };
        let h: JobHandle = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        assert!(matches!(block_on(f.poll(&h)).unwrap(), JobProgress::Running { .. }));
        let JobProgress::Done(done) = block_on(f.poll(&h)).unwrap() else { panic!() };
        assert_eq!(done.outputs.len(), if video { 1 } else { 10 });
        block_on(f.cancel(&h)).unwrap();
        let requests = wire.requests.lock().unwrap();
        for r in requests.iter().filter(|r| r.url.starts_with("https://v3.fal.media")) {
            assert!(r.headers.is_empty());
        }
        assert_eq!(requests.last().unwrap().method, HttpMethod::Put);
        let mut bad = h;
        bad.data["status_url"] = json!("https://queue.fal.run/xai/other/requests/r_1/status");
        assert!(Record::read(&bad, &Secret::new(KEY).unwrap()).is_err());
    }
}

#[test]
fn grok31_possible_moderation_charges_and_lost_submissions_never_prove_no_charge() {
    for status in [400, 401, 402, 403, 422, 429, 500, 503] {
        let wire = Wire::new(vec![HttpResponse { status, headers: vec![("Retry-After".into(), "2".into())], body: KEY.as_bytes().to_vec() }]);
        let context = crate::SubmitContext::new();
        let e = block_on(fal(&wire).submit_with_delivery(&grok(GROK_IMAGE), &context)).unwrap_err();
        assert!(!e.message.contains(KEY));
        assert!(context.finish_error().is_none(), "no documented no-charge proof, including moderation 422");
        if status == 429 {
            assert_eq!(e.retry_after_ms, Some(2000));
        }
    }
    let wire = Arc::new(Wire { replies: Mutex::new(vec![Err(ProviderError::new(ErrorKind::Network, KEY))].into()), ..Default::default() });
    let context = crate::SubmitContext::new();
    let e = block_on(fal(&wire).submit_with_delivery(&grok(GROK_VIDEO), &context)).unwrap_err();
    assert!(!e.message.contains(KEY));
    assert!(context.finish_error().is_none());
    let wire = Wire::new(vec![reply(json!({"status":"COMPLETED","error":"content policy violation"}))]);
    assert!(matches!(block_on(fal(&wire).poll(&handle(GROK_IMAGE))).unwrap(), JobProgress::Failed(_)));
}

#[test]
fn grok31_host_quotas_are_model_scoped_and_large_video_is_downloadable() {
    let defaults = Limits::default();
    for model in [Model::Image, Model::Photon, Model::LtxText] {
        let limits = defaults.for_model(model);
        assert_eq!(limits.request_bytes, Limits::HARD.request_bytes);
        assert_eq!(limits.total_pixels, Limits::HARD.total_pixels);
    }
    let edit = defaults.for_model(Model::GrokEdit);
    assert_eq!(edit.request_bytes, 8 * 1024 * 1024);
    assert_eq!(edit.total_output_bytes, 10 * Limits::HARD.output_bytes);
    let mut clip = include_bytes!("fixtures/blue.mp4").to_vec();
    let padding = 17 * 1024 * 1024usize;
    clip.extend_from_slice(&u32::try_from(padding + 8).unwrap().to_be_bytes());
    clip.extend_from_slice(b"free");
    clip.resize(clip.len() + padding, 0);
    let wire = Wire::new(vec![
        reply(json!({"status":"COMPLETED"})),
        reply(json!({"video":{"url":"https://fal.media/grok.mp4","file_size":clip.len()}})),
        HttpResponse { status: 200, headers: vec![], body: clip },
    ]);
    assert!(matches!(block_on(fal(&wire).poll(&handle(GROK_VIDEO))).unwrap(), JobProgress::Done(_)));
    let limits = defaults.for_model(Model::GrokText);
    assert_eq!(limits.output_bytes, 256 * 1024 * 1024);
    let record = Record::read(&handle(GROK_VIDEO), &Secret::new(KEY).unwrap()).unwrap().0;
    for value in [
        json!({"video":{"url":"https://evil.example/clip"}}),
        json!({"video":{"url":"https://fal.media/clip","file_size":268435457u64}}),
        json!({"video":{"url":"https://fal.media/clip"},"images":[]}),
    ] {
        assert!(response::outputs(&serde_json::to_vec(&value).unwrap(), record, limits, &Secret::new(KEY).unwrap()).is_err());
    }
}

#[test]
fn grok31_five_near_limit_edit_references_fit_the_larger_request_cap() {
    // Deterministic noise makes an authored PNG approach the per-reference byte cap.
    let mut seed = 31u32;
    let noise: Vec<u8> = (0..512 * 500 * 4)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 24) as u8
        })
        .collect();
    let png = nodecraft_media::encode_png(512, 500, &noise, &Default::default()).unwrap();
    assert!(png.len() > 900_000 && png.len() <= Limits::HARD.input_bytes);
    let mut j = job(GROK_EDIT);
    j.inputs.extend((0..5).map(|_| crate::JobInput::Media { role: "images".into(), mime: "image/png".into(), bytes: png.clone().into() }));
    let (_, body) = request::prepare(&j, Limits::default()).unwrap();
    assert!(body.len() > Limits::HARD.request_bytes && body.len() < Limits::GROK_HARD.request_bytes);
    assert!(request::prepare(&j, Limits { request_bytes: body.len(), ..Limits::default() }).is_ok());
    assert!(request::prepare(&j, Limits { request_bytes: body.len() - 1, ..Limits::default() }).is_err());
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[test]
#[ignore = "paid live smoke: NODECRAFT_LIVE_GROK=1 and FAL_KEY; one low-quality 1K image costs $0.04"]
fn grok31_live_smoke() {
    if std::env::var("NODECRAFT_LIVE_GROK").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("FAL_KEY").unwrap()).unwrap();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let f = Fal::new(key, Arc::new(crate::http::ReqwestHttp::new().unwrap()));
        let mut j = grok(GROK_IMAGE);
        j.params.insert("image_preset".into(), json!("low-1k"));
        let Submission::Queued(h) = f.submit(&j).await.unwrap() else { panic!() };
        for _ in 0..180 {
            match f.poll(&h).await.unwrap() {
                JobProgress::Done(done) => {
                    assert_eq!(done.outputs.len(), 1);
                    return;
                }
                JobProgress::Failed(error) => panic!("{}", error.message),
                _ => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
            }
        }
        let _ = f.cancel(&h).await;
        panic!("live Grok task timed out");
    });
}
