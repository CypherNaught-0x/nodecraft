//! LTX Pro's public HTTP schemas through fal's actual bounded queue adapter. Offline.
use super::tests::{handle, job, png, receipt};
use super::*;
use futures::executor::block_on;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};
const KEY: &str = "ltx30-fixture-not-a-key";
const CLIP: &[u8] = include_bytes!("fixtures/blue.mp4");
#[derive(Default)]
struct Wire {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<HttpResponse>>,
}
impl Wire {
    fn new(replies: Vec<HttpResponse>) -> Arc<Self> {
        Arc::new(Self { replies: Mutex::new(replies.into()), ..Default::default() })
    }
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable transport refused")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { Ok(reply) })
    }
}
fn reply(value: Value) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&value).unwrap() }
}
fn fal(wire: &Arc<Wire>) -> Fal {
    Fal::new(Secret::new(KEY).unwrap(), wire.clone())
}
fn frame(role: &str) -> crate::JobInput {
    crate::JobInput::Media { role: role.into(), mime: "image/png".into(), bytes: png(2, 2).into() }
}
fn ltx(model: &str) -> ProviderJob {
    let mut j = job(model);
    if model == LTX_I2V {
        j.inputs.push(frame("first_frame"));
    }
    j
}

#[test]
fn ltx30_both_routes_use_exact_pro_schema_and_optional_end_frame() {
    for model in [LTX, LTX_I2V] {
        let mut j = ltx(model);
        if model == LTX_I2V {
            j.inputs.push(frame("last_frame"));
        }
        let wire = Wire::new(vec![reply(receipt(model))]);
        let Submission::Queued(h) = block_on(fal(&wire).submit(&j)).unwrap() else { panic!() };
        assert_eq!(h, handle(model));
        let requests = wire.requests.lock().unwrap();
        let request = &requests[0];
        assert_eq!(request.url, format!("{ORIGIN}/{model}"));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let mut expected = json!({"prompt":"A blue circle","duration":6,"resolution":"1080p","aspect_ratio":"16:9","fps":25,"generate_audio":true,"enable_safety_checker":true});
        if model == LTX_I2V {
            use base64::Engine;
            let uri = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png(2, 2)));
            expected["image_url"] = json!(uri);
            expected["end_image_url"] = expected["image_url"].clone();
        }
        assert_eq!(body, expected);
        assert!(request.headers.iter().any(|(k, v)| k == "Authorization" && v == &format!("Key {KEY}")));
        assert!(request.headers.iter().any(|(k, v)| k == "X-Fal-No-Retry" && v == "1"));
        assert!(!fal(&wire).retry_ambiguous_submit());
    }
}

#[test]
fn ltx30_all_advertised_duration_resolution_ratio_and_fps_choices_are_numeric_on_wire() {
    for model in [LTX, LTX_I2V] {
        for duration in [6, 8, 10] {
            for resolution in ["1080p", "1440p", "2160p"] {
                for aspect in ["16:9", "9:16"] {
                    for fps in ["24", "25", "48", "50"] {
                        let mut j = ltx(model);
                        j.params = serde_json::from_value(
                            json!({"duration_seconds":duration,"resolution":resolution,"aspect_ratio":aspect,"fps":fps,"generate_audio":false}),
                        )
                        .unwrap();
                        let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
                        let body: Value = serde_json::from_slice(&bytes).unwrap();
                        assert_eq!(body["duration"], duration);
                        assert_eq!(body["resolution"], resolution);
                        assert_eq!(body["aspect_ratio"], aspect);
                        assert_eq!(body["fps"].as_u64(), Some(fps.parse().unwrap()));
                        assert_eq!(body["generate_audio"], false);
                    }
                }
            }
        }
    }
}

#[test]
fn ltx30_bad_roles_values_and_frames_refuse_before_http() {
    let mut jobs = vec![];
    for (key, value) in [
        ("duration_seconds", json!(7)),
        ("duration_seconds", json!("6")),
        ("duration_seconds", json!(20)),
        ("fps", json!(25)),
        ("fps", json!("30")),
        ("resolution", json!("720p")),
        ("aspect_ratio", json!("auto")),
        ("generate_audio", json!("true")),
        ("seed", json!(1)),
        ("model", json!("ltx-2-5-pro")),
        ("resolution", json!({"nested":[]})),
    ] {
        let mut j = ltx(LTX);
        j.params.insert(key.into(), value);
        jobs.push(j);
    }
    jobs.push(job(LTX_I2V));
    let mut j = ltx(LTX);
    j.inputs.push(frame("first_frame"));
    jobs.push(j);
    let mut j = ltx(LTX_I2V);
    j.inputs.push(frame("first_frame"));
    jobs.push(j);
    let mut j = ltx(LTX);
    j.inputs.push(j.inputs[0].clone());
    jobs.push(j);
    let mut j = ltx(LTX);
    j.inputs.push(crate::JobInput::Text { role: "negative_prompt".into(), text: "ignored?".into() });
    jobs.push(j);
    for text in [" ".into(), "x".repeat(5001)] {
        let mut j = ltx(LTX);
        j.inputs = vec![crate::JobInput::Text { role: "prompt".into(), text }];
        jobs.push(j);
    }
    for (mime, bytes) in [("image/png", b"bad image".to_vec()), ("image/jpeg", png(2, 2)), ("image/png", png(1025, 1))] {
        let mut j = ltx(LTX_I2V);
        j.inputs[1] = crate::JobInput::Media { role: "first_frame".into(), mime: mime.into(), bytes: bytes.into() };
        jobs.push(j);
    }
    let wire = Arc::new(Wire::default());
    for j in jobs {
        assert_eq!(block_on(fal(&wire).submit(&j)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    }
    assert!(wire.requests.lock().unwrap().is_empty());
}

#[test]
fn ltx30_escaped_json_and_encoded_images_obey_exact_limits_before_codecs() {
    let mut j = ltx(LTX_I2V);
    j.inputs.push(frame("last_frame"));
    j.inputs[0] = crate::JobInput::Text { role: "prompt".into(), text: "\"\n\\".repeat(200) };
    let (_, body) = request::prepare(&j, Limits::default()).unwrap();
    assert!(request::prepare(&j, Limits { request_bytes: body.len(), ..Limits::default() }).is_ok());
    assert!(request::prepare(&j, Limits { request_bytes: body.len() - 1, ..Limits::default() }).is_err());
    assert!(request::prepare(&j, Limits { output_width: 1919, ..Limits::default() }).is_err());
    assert!(request::prepare(&j, Limits { total_pixels: 1920 * 1080 - 1, ..Limits::default() }).is_err());
    assert!(request::prepare(&j, Limits { input_bytes: 1, ..Limits::default() }).is_err());
}

#[test]
fn ltx30_saved_tasks_poll_resume_cancel_and_download_without_credentials() {
    for model in [LTX, LTX_I2V] {
        let wire = Wire::new(vec![
            reply(receipt(model)),
            reply(json!({"status":"IN_QUEUE","queue_position":2})),
            reply(json!({"status":"IN_PROGRESS"})),
            reply(json!({"status":"COMPLETED"})),
            reply(json!({"video":{"url":"https://v3.fal.media/files/ltx.mp4","content_type":"video/mp4","file_size":CLIP.len()}})),
            HttpResponse { status: 200, headers: vec![], body: CLIP.to_vec() },
            HttpResponse { status: 202, headers: vec![], body: serde_json::to_vec(&json!({"status":"CANCELLATION_REQUESTED"})).unwrap() },
        ]);
        let f = fal(&wire);
        let Submission::Queued(h) = block_on(f.submit(&ltx(model))).unwrap() else { panic!() };
        let h: JobHandle = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        assert!(matches!(block_on(f.poll(&h)).unwrap(), JobProgress::Queued { position: Some(2) }));
        assert!(matches!(block_on(f.poll(&h)).unwrap(), JobProgress::Running { .. }));
        let JobProgress::Done(done) = block_on(f.poll(&h)).unwrap() else { panic!() };
        assert_eq!(done.outputs, vec![Output::Bytes { mime: "video/mp4".into(), bytes: CLIP.into() }]);
        assert!(done.usage.cost.is_none());
        block_on(f.cancel(&h)).unwrap();
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests[1].url, format!("{ORIGIN}/fal-ai/ltx-2.3/requests/r_1/status"));
        assert!(requests[5].headers.is_empty());
        assert_eq!(requests[6].method, HttpMethod::Put);
        assert_eq!(requests[6].url, format!("{ORIGIN}/fal-ai/ltx-2.3/requests/r_1/cancel"));
    }
}

#[test]
fn ltx30_result_envelopes_receipts_and_video_bounds_are_not_remote_authority() {
    let key = Secret::new(KEY).unwrap();
    let h = handle(LTX);
    let record = Record::read(&h, &key).unwrap().0;
    let limits = Limits::default().for_model(Model::LtxText);
    assert_eq!(limits.output_bytes, 256 * 1024 * 1024);
    assert_eq!(limits.total_output_bytes, limits.output_bytes);
    for value in [
        json!({"images":[]}),
        json!({"video":{"url":"https://evil.example/clip"}}),
        json!({"video":{"url":"https://fal.media/clip","content_type":"image/png"}}),
        json!({"video":{"url":"https://fal.media/clip","file_size":268435457u64}}),
        json!({"video":{"url":"https://fal.media/clip"},"audio":{}}),
    ] {
        assert!(response::outputs(&serde_json::to_vec(&value).unwrap(), record, limits, &key).is_err());
    }
    let mut forged = h;
    forged.data["status_url"] = json!("https://queue.fal.run/fal-ai/wan/requests/r_1/status");
    assert!(Record::read(&forged, &key).is_err());
    assert!(media::validate(b"not mp4", record, limits, &mut 0).is_err());
    let lowered = Limits { output_bytes: 1, total_output_bytes: 1, ..limits };
    assert!(
        response::outputs(&serde_json::to_vec(&json!({"video":{"url":"https://fal.media/clip","file_size":CLIP.len()}})).unwrap(), record, lowered, &key)
            .is_err()
    );
    assert_eq!(Limits::default().for_model(Model::Image).output_bytes, Limits::HARD.output_bytes);
}

#[test]
fn ltx30_supported_video_can_download_past_the_image_quota() {
    // A valid MP4 container with a padding atom, not a paid generated clip.
    let mut clip = CLIP.to_vec();
    let padding = 17 * 1024 * 1024usize;
    clip.extend_from_slice(&u32::try_from(padding + 8).unwrap().to_be_bytes());
    clip.extend_from_slice(b"free");
    clip.resize(clip.len() + padding, 0);
    assert!(clip.len() > Limits::HARD.output_bytes);
    let wire = Wire::new(vec![
        reply(receipt(LTX)),
        reply(json!({"status":"COMPLETED"})),
        reply(json!({"video":{"url":"https://v3.fal.media/files/large-ltx.mp4","file_size":clip.len()}})),
        HttpResponse { status: 200, headers: vec![], body: clip },
    ]);
    let f = fal(&wire);
    let Submission::Queued(h) = block_on(f.submit(&ltx(LTX))).unwrap() else { panic!() };
    let JobProgress::Done(done) = block_on(f.poll(&h)).unwrap() else { panic!() };
    assert!(matches!(done.outputs.as_slice(),[Output::Bytes{mime,bytes}] if mime == "video/mp4" && bytes.len()>Limits::HARD.output_bytes));
    assert!(wire.requests.lock().unwrap()[3].headers.is_empty());
}

#[test]
fn ltx30_error_kinds_and_retry_hints_do_not_echo_provider_secrets() {
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
        let error = block_on(fal(&wire).submit(&ltx(LTX))).unwrap_err();
        assert_eq!(error.kind, kind);
        assert!(!error.message.contains(KEY));
        if status == 429 {
            assert_eq!(error.retry_after_ms, Some(2000));
        }
    }
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
#[test]
#[ignore = "paid live smoke: NODECRAFT_LIVE_LTX=1 and FAL_KEY; one6secondPro1080pclip;priceunknown"]
fn ltx30_live_smoke() {
    if std::env::var("NODECRAFT_LIVE_LTX").as_deref() != Ok("1") {
        return;
    }
    let key = Secret::new(&std::env::var("FAL_KEY").unwrap()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let f = Fal::new(key, Arc::new(crate::http::ReqwestHttp::new().unwrap()));
        let Submission::Queued(h) = f.submit(&ltx(LTX)).await.unwrap() else { panic!() };
        for _ in 0..180 {
            match f.poll(&h).await.unwrap() {
                JobProgress::Done(done) => {
                    assert!(matches!(done.outputs.as_slice(),[Output::Bytes{mime,..}] if mime=="video/mp4"));
                    return;
                }
                JobProgress::Failed(e) => panic!("{}", e.message),
                _ => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
            }
        }
        let _ = f.cancel(&h).await;
        panic!("live LTX request timed out");
    });
}
