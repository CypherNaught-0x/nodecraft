//! Original offline protocol fixtures. No account, network, vendor media or paid calls.
use super::*;
use futures::executor::block_on;
use serde_json::json;
use std::{collections::VecDeque, sync::Mutex};

const KEY: &str = "magnific38-public-fixture-not-a-key";
const ID: &str = "00000000-0038-4000-8000-000000000001";
const URL: &str = "https://ai-statics.freepik.com/public/original.png";
#[derive(Default)]
struct Wire {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<Result<crate::HttpResponse, ProviderError>>>,
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<crate::HttpResponse, ProviderError>> {
        panic!("redirect-capable request refused")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<crate::HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { reply })
    }
}
fn reply(status: u16, body: serde_json::Value) -> Result<crate::HttpResponse, ProviderError> {
    Ok(crate::HttpResponse { status, headers: vec![], body: serde_json::to_vec(&body).unwrap() })
}
fn task(state: &str) -> serde_json::Value {
    json!({"data":{"task_id":ID,"status":state,"generated":if state=="COMPLETED" {vec![URL]} else {vec![]}}})
}
fn png(width: u32, height: u32) -> Vec<u8> {
    nodecraft_media::encode_png(width, height, &vec![120; usize::try_from(width * height * 4).unwrap()], &Default::default()).unwrap()
}
fn job() -> ProviderJob {
    ProviderJob {
        model: PRECISION_V2.into(),
        params: Default::default(),
        inputs: vec![crate::JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: png(2, 3).into() }],
        idempotency_key: "public-magnific38-operation".into(),
    }
}
fn key() -> Secret {
    Secret::new(KEY).unwrap()
}
fn provider(wire: &Arc<Wire>) -> Magnific {
    Magnific::new(key(), wire.clone())
}
fn handle() -> JobHandle {
    request::prepare(&job(), Limits::default()).unwrap().0.handle(ID)
}
#[test]
fn magnific38_raw_base64_native_request_and_both_typed_scale_forms() {
    use base64::Engine;
    for value in [json!(2), json!(4), json!("2"), json!("4")] {
        let mut j = job();
        j.params.insert("scale_factor".into(), value.clone());
        let (contract, bytes) = request::prepare(&j, Limits::default()).unwrap();
        let scale = if value == json!(2) || value == json!("2") { 2 } else { 4 };
        assert_eq!((contract.width, contract.height), (2 * scale, 3 * scale));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            json!({"image":base64::engine::general_purpose::STANDARD.encode(png(2,3)),"scale_factor":scale,"flavor":"photo","sharpen":7,"smart_grain":7,"ultra_detail":30,"filter_nsfw":true})
        );
    }
}
#[test]
fn magnific38_bad_fields_and_inputs_are_proven_unsubmitted() {
    let mut cases = vec![];
    for (name, value) in [
        ("scale_factor", json!(3)),
        ("scale_factor", json!("02")),
        ("scale_factor", json!(2.5)),
        ("scale_factor", json!(16)),
        ("sharpen", json!(-1)),
        ("sharpen", json!(101)),
        ("smart_grain", json!(true)),
        ("ultra_detail", json!({})),
        ("flavor", json!("sublime")),
        ("filter_nsfw", json!(false)),
        ("prompt", json!("invented")),
        ("seed", json!(42)),
        ("output_format", json!("png")),
    ] {
        let mut j = job();
        j.params.insert(name.into(), value);
        cases.push(j);
    }
    let mut j = job();
    j.inputs.clear();
    cases.push(j);
    let mut j = job();
    j.inputs.push(j.inputs[0].clone());
    cases.push(j);
    let mut j = job();
    j.inputs = vec![crate::JobInput::Text { role: "image".into(), text: "not an image".into() }];
    cases.push(j);
    for (mime, bytes) in [
        ("image/svg+xml", b"<svg/>".to_vec()),
        ("image/png", vec![0; Limits::HARD.input_bytes + 1]),
        ("image/jpeg", png(2, 3)),
        ("image/png", png(1025, 1)),
        ("image/jpeg", include_bytes!("../fal/fixtures/oriented.jpg").to_vec()),
    ] {
        let mut j = job();
        j.inputs = vec![crate::JobInput::Media { role: "image".into(), mime: mime.into(), bytes: bytes.into() }];
        cases.push(j);
    }
    for j in cases {
        let w = Arc::new(Wire::default());
        let context = SubmitContext::new();
        assert_eq!(block_on(provider(&w).submit_with_delivery(&j, &context)).unwrap_err().kind, ErrorKind::UnsupportedInput);
        assert!(context.finish_error().is_some());
        assert!(w.requests.lock().unwrap().is_empty());
    }
}
#[test]
fn magnific38_exact_extent_and_host_geometry_precede_payment() {
    let j = job();
    let (_, bytes) = request::prepare(&j, Limits::default()).unwrap();
    assert!(request::prepare(&j, Limits { request_bytes: bytes.len(), ..Limits::default() }).is_ok());
    for limits in [
        Limits { request_bytes: bytes.len() - 1, ..Limits::default() },
        Limits { output_width: 3, ..Limits::default() },
        Limits { output_height: 5, ..Limits::default() },
        Limits { total_pixels: 23, ..Limits::default() },
        Limits { output_bytes: 0, ..Limits::default() },
    ] {
        let w = Arc::new(Wire::default());
        assert!(block_on(Magnific::with_limits(key(), w.clone(), limits).unwrap().submit(&j)).is_err());
        assert!(w.requests.lock().unwrap().is_empty());
    }
}
#[test]
fn magnific38_output_decoder_budget_is_admitted_before_payment() {
    let limits = Limits { codec_bytes: 128 * 1024, ..Limits::default() };
    let input = png(128, 128);
    // This is an output-admission failure, not an input that cannot be decoded.
    assert!(nodecraft_media::decode_image(&input, &limits.image(true)).is_ok());
    let mut j = job();
    j.inputs = vec![crate::JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: input.into() }];
    j.params.insert("scale_factor".into(), json!(4));
    let wire = Arc::new(Wire::default());
    let context = SubmitContext::new();
    let error = block_on(Magnific::with_limits(key(), wire.clone(), limits).unwrap().submit_with_delivery(&j, &context)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::UnsupportedInput);
    assert!(context.finish_error().is_some());
    assert!(wire.requests.lock().unwrap().is_empty());

    let needed = 512 * 512 * 16;
    assert!(request::prepare(&j, Limits { codec_bytes: needed - 1, ..Limits::default() }).is_err());
    assert!(request::prepare(&j, Limits { codec_bytes: needed, ..Limits::default() }).is_ok());
    // The supported maximum geometry still fits the model-specific default.
    j.inputs = vec![crate::JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: png(1024, 1024).into() }];
    let (contract, _) = request::prepare(&j, Limits::default()).unwrap();
    assert_eq!((contract.width, contract.height), (4096, 4096));
}
#[test]
fn magnific38_queue_recreated_provider_keeps_exact_bytes_and_strips_cdn_credentials() {
    let image = png(4, 6);
    let w = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([
            reply(200, task("CREATED")),
            reply(200, task("IN_PROGRESS")),
            reply(200, task("COMPLETED")),
            Ok(crate::HttpResponse { status: 200, headers: vec![], body: image.clone() }),
        ])),
        ..Default::default()
    });
    let Submission::Queued(h) = block_on(provider(&w).submit(&job())).unwrap() else { panic!() };
    let restored: JobHandle = serde_json::from_slice(&serde_json::to_vec(&h).unwrap()).unwrap();
    assert!(matches!(block_on(provider(&w).poll(&restored)).unwrap(), JobProgress::Running { .. }));
    let JobProgress::Done(done) = block_on(provider(&w).poll(&restored)).unwrap() else { panic!() };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: "image/png".into(), bytes: image.into() }]);
    assert!(done.usage.cost.is_none());
    let requests = w.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].url, ROOT);
    assert_eq!(requests[1].url, format!("{ROOT}/{ID}"));
    assert_eq!(requests[3].url, URL);
    assert!(requests[3].headers.is_empty());
    for request in requests.iter().take(3) {
        assert!(request.headers.iter().any(|(name, value)| name == "x-magnific-api-key" && value == KEY));
        assert!(!format!("{request:?}").contains(KEY));
    }
    assert!(!provider(&w).retry_ambiguous_submit());
}
#[test]
fn magnific38_documented_refusals_and_ambiguous_submit_statuses_keep_distinct_evidence() {
    for status in [400, 401, 402, 403, 429, 500, 503] {
        let w = Arc::new(Wire { replies: Mutex::new(VecDeque::from([reply(status, json!({"message":"Invalid request fixture"}))])), ..Default::default() });
        let context = SubmitContext::new();
        assert!(block_on(provider(&w).submit_with_delivery(&job(), &context)).is_err());
        assert_eq!(context.finish_error().is_some(), matches!(status, 400 | 401));
        assert_eq!(w.requests.lock().unwrap().len(), 1);
    }
    for body in
        [json!({"message":"Invalid API key","data":{"task_id":ID}}), json!({"message":null}), json!({"message":""}), json!({"message":"x".repeat(4097)})]
    {
        let w = Arc::new(Wire { replies: Mutex::new(VecDeque::from([reply(401, body)])), ..Default::default() });
        let context = SubmitContext::new();
        assert!(block_on(provider(&w).submit_with_delivery(&job(), &context)).is_err());
        assert!(context.finish_error().is_none());
    }
    let w = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([Err(ProviderError::new(ErrorKind::Network, format!("must redact {KEY}")))])),
        ..Default::default()
    });
    let context = SubmitContext::new();
    let error = block_on(provider(&w).submit_with_delivery(&job(), &context)).unwrap_err();
    assert!(!error.message.contains(KEY));
    assert!(context.finish_error().is_none());
}
#[test]
fn magnific38_invalid_tasks_and_delivery_routes_refuse_before_image_get() {
    for body in [
        json!({"data":{"task_id":ID,"status":"UNKNOWN","generated":[]}}),
        json!({"data":{"task_id":"../foreign","status":"COMPLETED","generated":[URL]}}),
        json!({"data":{"task_id":ID,"status":"IN_PROGRESS","generated":[URL]}}),
        json!({"data":{"task_id":ID,"status":"COMPLETED","generated":[URL,URL]}}),
    ] {
        let w = Arc::new(Wire { replies: Mutex::new(VecDeque::from([reply(200, body)])), ..Default::default() });
        assert!(block_on(provider(&w).poll(&handle())).is_err());
        assert_eq!(w.requests.lock().unwrap().len(), 1);
    }
    for url in [
        "http://ai-statics.freepik.com/x",
        "https://ai-statics.freepik.com.evil.test/x",
        "https://ai-statics.freepik.com:443/x",
        "https://AI-statics.freepik.com/x",
        "https://ai-statics.freepik.com/a/../x",
        "https://user@ai-statics.freepik.com/x",
        "https://ai-statics.freepik.com/x#fragment",
    ] {
        let body = json!({"data":{"task_id":ID,"status":"COMPLETED","generated":[url]}});
        let w = Arc::new(Wire { replies: Mutex::new(VecDeque::from([reply(200, body)])), ..Default::default() });
        assert!(block_on(provider(&w).poll(&handle())).is_err());
        assert_eq!(w.requests.lock().unwrap().len(), 1);
    }
}
#[test]
fn magnific38_bad_actual_image_or_expired_link_never_becomes_a_new_submit() {
    for image in [png(4, 5), b"not an image".to_vec(), {
        let mut image = png(4, 6);
        image.push(0);
        image
    }] {
        let w = Arc::new(Wire {
            replies: Mutex::new(VecDeque::from([reply(200, task("COMPLETED")), Ok(crate::HttpResponse { status: 200, headers: vec![], body: image })])),
            ..Default::default()
        });
        assert!(block_on(provider(&w).poll(&handle())).is_err());
        assert!(w.requests.lock().unwrap().iter().all(|request| request.method == HttpMethod::Get));
    }
    let w =
        Arc::new(Wire { replies: Mutex::new(VecDeque::from([reply(200, task("COMPLETED")), reply(403, json!({"message":"expired"}))])), ..Default::default() });
    assert_eq!(block_on(provider(&w).poll(&handle())).unwrap_err().kind, ErrorKind::Expired);
    assert_eq!(w.requests.lock().unwrap().len(), 2);
}
#[test]
fn magnific38_restored_bounds_and_unsupported_cancel_never_send() {
    for (field, value) in [("width", json!(0)), ("height", json!(4097)), ("v", json!(2)), ("model", json!("invented")), ("limits", json!({}))] {
        let mut h = handle();
        h.data[field] = value;
        let w = Arc::new(Wire::default());
        assert!(block_on(provider(&w).poll(&h)).is_err());
        assert!(block_on(provider(&w).cancel(&h)).is_err());
        assert!(w.requests.lock().unwrap().is_empty());
    }
    let w = Arc::new(Wire::default());
    assert!(block_on(provider(&w).cancel(&handle())).is_err());
    assert!(w.requests.lock().unwrap().is_empty());
    let w = Arc::new(Wire::default());
    let f = Magnific::with_limits(key(), w.clone(), Limits { output_width: 3, ..Limits::default() }).unwrap();
    assert!(block_on(f.poll(&handle())).is_err());
    assert!(w.requests.lock().unwrap().is_empty());
}
#[test]
fn magnific38_secret_header_is_redacted_even_without_explicit_marking() {
    let request = HttpRequest::get(ROOT).header("x-magnific-api-key", KEY);
    assert!(!format!("{request:?}").contains(KEY));
}

#[test]
fn magnific38_documented_problem_details_and_malformed_refusals_are_bounded() {
    let body = json!({"problem":{"message":"Validation error","invalid_params":[{"name":"image","field":"image","reason":"Invalid image"}]}});
    assert!(response::refusal(&reply(400, body).unwrap(), Limits::default()));
    for (headers, body) in [
        (vec![], b"{\"message\":\"bad\",\"message\":\"other\"}".to_vec()),
        (vec![("content-type".into(), "text/html".into())], serde_json::to_vec(&json!({"message":"Invalid API key"})).unwrap()),
        (vec![("content-length".into(), "0".into())], serde_json::to_vec(&json!({"message":"Invalid API key"})).unwrap()),
    ] {
        assert!(!response::refusal(&crate::HttpResponse { status: 401, headers, body }, Limits::default()));
    }
    let mut response = reply(429, json!({"message":"limited"})).unwrap();
    response.headers = vec![("Retry-After".into(), "2".into())];
    let error = response::status(&response, Limits::default().response_bytes).unwrap_err();
    assert_eq!(error.retry_after_ms, Some(2000));
    response.headers.push(("retry-after".into(), "4".into()));
    assert!(response::status(&response, Limits::default().response_bytes).unwrap_err().retry_after_ms.is_none());
}
#[test]
fn magnific38_valid_large_png_and_recorded_lower_caps_survive_resume() {
    use image::ImageEncoder;
    let mut raw = vec![0u8; 2048 * 2048 * 6];
    let mut state = 38u32;
    for byte in &mut raw {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *byte = state as u8;
    }
    let mut image = Vec::new();
    image::codecs::png::PngEncoder::new(&mut image).write_image(&raw, 2048, 2048, image::ExtendedColorType::Rgb16).unwrap();
    drop(raw);
    assert!(image.len() > 16 * 1024 * 1024);
    let contract = Contract { width: 2048, height: 2048, limits: Limits::default() };
    let h = contract.handle(ID);
    let w = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([reply(200, task("COMPLETED")), Ok(crate::HttpResponse { status: 200, headers: vec![], body: image.clone() })])),
        ..Default::default()
    });
    let JobProgress::Done(done) = block_on(provider(&w).poll(&h)).unwrap() else { panic!() };
    let [Output::Bytes { mime, bytes }] = done.outputs.as_slice() else { panic!() };
    assert_eq!(mime, "image/png");
    assert!(bytes.as_ref() == image.as_slice(), "original PNG bytes are retained");
    assert_eq!(w.requests.lock().unwrap()[1].max_response_bytes, 256 * 1024 * 1024);
    let h = Contract { limits: Limits { output_bytes: 1024, ..Limits::default() }, ..contract }.handle(ID);
    let w = Arc::new(Wire {
        replies: Mutex::new(VecDeque::from([reply(200, task("COMPLETED")), Ok(crate::HttpResponse { status: 200, headers: vec![], body: image })])),
        ..Default::default()
    });
    assert!(block_on(provider(&w).poll(&h)).is_err());
    assert_eq!(w.requests.lock().unwrap()[1].max_response_bytes, 1024);
}

#[test]
fn magnific38_upright_jpeg_and_webp_keep_original_inputs_and_decode_scaled_outputs() {
    use base64::Engine;
    use image::ImageEncoder;
    for mime in ["image/jpeg", "image/webp"] {
        let encode = |width, height| {
            let mut bytes = Vec::new();
            let pixels = vec![128; usize::try_from(width * height * 3).unwrap()];
            if mime == "image/jpeg" {
                image::codecs::jpeg::JpegEncoder::new(&mut bytes).encode(&pixels, width, height, image::ExtendedColorType::Rgb8).unwrap();
            } else {
                image::codecs::webp::WebPEncoder::new_lossless(&mut bytes).write_image(&pixels, width, height, image::ExtendedColorType::Rgb8).unwrap();
            }
            bytes
        };
        let input = encode(2, 3);
        let output = encode(4, 6);
        let mut j = job();
        j.inputs = vec![crate::JobInput::Media { role: "image".into(), mime: mime.into(), bytes: input.clone().into() }];
        let (contract, body) = request::prepare(&j, Limits::default()).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["image"], base64::engine::general_purpose::STANDARD.encode(input));
        assert_eq!(media::validate(&output, contract).unwrap(), mime);
    }
}
#[test]
fn magnific38_uuid_shaped_secret_echo_never_enters_a_handle_or_poll_url() {
    let w = Arc::new(Wire { replies: Mutex::new(VecDeque::from([reply(200, task("CREATED"))])), ..Default::default() });
    let f = Magnific::new(Secret::new(ID).unwrap(), w.clone());
    let error = block_on(f.submit(&job())).unwrap_err();
    assert!(!error.message.contains(ID));
    assert!(block_on(f.poll(&handle())).is_err());
    assert_eq!(w.requests.lock().unwrap().len(), 1);
}

#[test]
fn magnific38_malformed_accepted_metadata_retains_canonical_recovery_authority() {
    for initial in [
        json!({"data":{"task_id":ID}}),
        json!({"data":{"task_id":ID,"status":"NEW_STATE","generated":{"bad":"shape"}}}),
        json!({"data":{"task_id":ID,"status":"COMPLETED","generated":["https://evil.test/not-authority"]}}),
    ] {
        let output = png(4, 6);
        let w = Arc::new(Wire {
            replies: Mutex::new(VecDeque::from([
                reply(200, initial),
                reply(200, task("COMPLETED")),
                Ok(crate::HttpResponse { status: 200, headers: vec![], body: output }),
            ])),
            ..Default::default()
        });
        let f = provider(&w);
        let Submission::Queued(h) = block_on(f.submit(&job())).unwrap() else { panic!() };
        assert_eq!(h.remote_id, ID);
        assert!(matches!(block_on(f.poll(&h)).unwrap(), JobProgress::Done(_)));
        let requests = w.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1].url, format!("{ROOT}/{ID}"));
        assert_eq!(requests[2].url, URL);
        assert_eq!(requests.iter().filter(|request| request.method == HttpMethod::Post).count(), 1);
    }
}

#[test]
fn magnific38_additive_task_metadata_is_safe_but_error_and_duplicate_authority_refuse() {
    let mut body = task("COMPLETED");
    body["request_metadata"] = json!({"version":2});
    body["data"]["created_at"] = json!("2026-10-27T00:00:00Z");
    body["data"]["error"] = json!(null);
    assert!(response::task(&serde_json::to_vec(&body).unwrap(), &key()).is_ok());
    for field in ["error", "problem"] {
        let mut body = task("COMPLETED");
        body[field] = json!({"message":"failed"});
        assert!(response::task(&serde_json::to_vec(&body).unwrap(), &key()).is_err());
        let mut body = task("COMPLETED");
        body["data"][field] = json!("failed");
        assert!(response::task(&serde_json::to_vec(&body).unwrap(), &key()).is_err());
    }
    let duplicated = format!("{{\"data\":{{\"task_id\":\"{ID}\",\"task_id\":\"{ID}\",\"status\":\"CREATED\",\"generated\":[]}}}}");
    assert!(response::receipt(duplicated.as_bytes(), &key()).is_err());
    assert!(response::task(duplicated.as_bytes(), &key()).is_err());
}
