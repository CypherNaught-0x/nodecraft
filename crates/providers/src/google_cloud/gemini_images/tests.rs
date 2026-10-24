use super::super::tests::{NOW, PROJECT, Transport, fixed_clock, granted, ok, stored};
use super::*;
use crate::{HttpMethod, HttpResponse, JobInput, Output};
use base64::Engine as _;
use futures::executor::block_on;
use serde_json::{Value, json};

const TOKEN: &str = "ya29.fixture-token";

fn png() -> Vec<u8> {
    sized_png(2, 2)
}
fn sized_png(width: u32, height: u32) -> Vec<u8> {
    let limits = nodecraft_media::Limits { max_width: width, max_height: height, ..nodecraft_media::Limits::default() };
    nodecraft_media::encode_png(width, height, &vec![7; width as usize * height as usize * 4], &limits).unwrap()
}
fn jpeg() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut bytes).encode(&[9; 2 * 2 * 3], 2, 2, image::ExtendedColorType::Rgb8).unwrap();
    bytes
}
fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
/// A reply as Google documents it: a few words, a thought, then the image.
fn made() -> Result<HttpResponse, ProviderError> {
    ok(json!({
        "candidates": [{"content": {"role": "model", "parts": [
            {"text": "Planning the composition.", "thought": true},
            {"inlineData": {"mimeType": "image/jpeg", "data": b64(&jpeg())}, "thought": true},
            {"text": "Here is your lighthouse."},
            {"inlineData": {"mimeType": "image/png", "data": b64(&png())}}
        ]}, "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 1130, "candidatesTokenCount": 1126, "thoughtsTokenCount": 210, "totalTokenCount": 2466},
        "modelVersion": FLASH
    }))
}
fn prompt(text: &str) -> JobInput {
    JobInput::Text { role: "prompt".into(), text: text.into() }
}
fn media(mime: &str, bytes: Vec<u8>) -> JobInput {
    JobInput::Media { role: "image".into(), mime: mime.into(), bytes: Arc::from(bytes) }
}
fn job(model: &str, inputs: Vec<JobInput>, params: Value) -> ProviderJob {
    let Value::Object(params) = params else { panic!("params are an object") };
    ProviderJob { model: model.into(), params, inputs, idempotency_key: "fixture".into() }
}
fn provider(t: &Arc<Transport>) -> GoogleGeminiImages {
    GoogleGeminiImages::new(&stored(), t.clone(), fixed_clock(NOW)).unwrap()
}
fn body(r: &crate::HttpRequest) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}
fn header<'a>(r: &'a crate::HttpRequest, name: &str) -> Option<&'a str> {
    r.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}
/// The generate request a job sends, after the token exchange, and what came of it.
fn sent(j: &ProviderJob) -> (Completed, crate::HttpRequest) {
    let t = Transport::new(vec![granted(TOKEN), made()]);
    let Submission::Finished(done) = block_on(provider(&t).submit(j)).unwrap() else { panic!("images are synchronous") };
    let requests = t.requests();
    assert_eq!(requests.len(), 2, "one token exchange, then one generation");
    let (redirects, r) = requests[1].clone();
    assert!(!redirects, "the paid request never follows a redirect");
    (done, r)
}
fn failed(reply: Value) -> ProviderError {
    let t = Transport::new(vec![granted(TOKEN), ok(reply)]);
    let scope = crate::SubmitContext::new();
    let error = block_on(provider(&t).submit_with_delivery(&job(FLASH, vec![prompt("a cat")], json!({})), &scope)).unwrap_err();
    assert!(scope.finish_error().is_none(), "a 200 reply may have been billed: {}", error.message);
    error
}

#[test]
fn a_prompt_and_references_send_the_documented_body_to_the_global_endpoint() {
    let j = job(
        PRO,
        vec![prompt("  A lighthouse at dusk  "), media("image/png", png()), media("image/jpeg", jpeg())],
        json!({"image_size": "4K", "aspect_ratio": "16:9"}),
    );
    let (done, r) = sent(&j);
    assert_eq!(
        (r.method, r.url.as_str()),
        (
            HttpMethod::Post,
            "https://aiplatform.googleapis.com/v1/projects/nodecraft-test/locations/global/publishers/google/models/gemini-3-pro-image:generateContent"
        )
    );
    assert_eq!(
        body(&r),
        json!({
            "contents": [{"role": "user", "parts": [
                {"text": "A lighthouse at dusk"},
                {"inlineData": {"mimeType": "image/png", "data": b64(&png())}},
                {"inlineData": {"mimeType": "image/jpeg", "data": b64(&jpeg())}}
            ]}],
            "generationConfig": {"responseModalities": ["TEXT", "IMAGE"], "candidateCount": 1, "imageConfig": {"aspectRatio": "16:9", "imageSize": "4K"}}
        })
    );
    // The token alone names the project: a quota-project header would demand a Service Usage role.
    assert_eq!(header(&r, "x-goog-user-project"), None);
    assert_eq!(header(&r, "user-agent"), Some(crate::USER_AGENT));
    assert!(r.headers.iter().any(|(n, v)| n == "Authorization" && v == &format!("Bearer {TOKEN}")));
    assert!(r.secret_headers.iter().any(|n| n == "authorization"), "the token travels only as a secret header");
    assert_eq!(done.outputs, vec![Output::Bytes { mime: "image/png".into(), bytes: Arc::from(png()) }], "thought images and text are left out");
    assert_eq!((done.usage.input_tokens, done.usage.output_tokens, done.usage.cost), (Some(1130), Some(1126), None), "tokens, charged at the catalog's rates");
}

#[test]
fn unset_settings_are_left_to_google_and_sizeless_models_send_none() {
    let (_, r) = sent(&job(FLASH, vec![prompt("a cat")], json!({})));
    assert_eq!(body(&r)["generationConfig"], json!({"responseModalities": ["TEXT", "IMAGE"], "candidateCount": 1}));
    let (_, r) = sent(&job(FLASH_LITE, vec![prompt("a cat")], json!({"image_size": "1K", "aspect_ratio": "4:5"})));
    assert_eq!(body(&r)["generationConfig"]["imageConfig"], json!({"aspectRatio": "4:5"}), "Flash-Lite makes 1K only");
    let (_, r) = sent(&job(FLASH, vec![prompt("a cat")], json!({"aspect_ratio": "1:8"})));
    assert_eq!(body(&r)["generationConfig"]["imageConfig"], json!({"aspectRatio": "1:8"}), "Flash alone takes the extreme ratios");
    let (_, r) = sent(&job(FLASH_2_5, vec![prompt("a cat"), media("image/png", png())], json!({"aspect_ratio": "21:9"})));
    assert!(r.url.ends_with("/models/gemini-2.5-flash-image:generateContent"));
    assert_eq!(body(&r)["generationConfig"]["imageConfig"], json!({"aspectRatio": "21:9"}));
    let (_, r) = sent(&job(NANO_BANANA_2_1, vec![prompt("a cat")], json!({"image_size": "2K"})));
    assert_eq!(body(&r)["generationConfig"]["imageConfig"], json!({"imageSize": "2K"}));
}

#[test]
fn a_job_outside_a_models_limits_is_refused_before_anything_is_sent() {
    let many = |n: usize| std::iter::once(prompt("a")).chain((0..n).map(|_| media("image/png", png()))).collect::<Vec<_>>();
    let mut gif = b"GIF89a".to_vec();
    gif.resize(32, 0);
    let cases = [
        (FLASH, vec![], json!({})),
        (FLASH, vec![prompt("   ")], json!({})),
        (FLASH, vec![prompt("a"), prompt("b")], json!({})),
        (FLASH, vec![prompt(&"x".repeat(32_001))], json!({})),
        (FLASH, many(15), json!({})),
        (FLASH_2_5, many(4), json!({})),
        (FLASH, vec![prompt("a"), media("image/gif", gif)], json!({})),
        (FLASH, vec![prompt("a"), media("image/jpeg", png())], json!({})),
        (FLASH, vec![prompt("a"), media("image/png", vec![])], json!({})),
        (
            FLASH,
            vec![
                prompt("a"),
                media("image/png", {
                    let mut b = png();
                    b.resize(7_000_001, 0);
                    b
                }),
            ],
            json!({}),
        ),
        (FLASH, vec![prompt("a"), JobInput::Media { role: "mask".into(), mime: "image/png".into(), bytes: Arc::from(png()) }], json!({})),
        (FLASH, vec![prompt("a")], json!({"image_size": "512"})),
        (FLASH, vec![prompt("a")], json!({"image_size": 1})),
        (FLASH_LITE, vec![prompt("a")], json!({"image_size": "2K"})),
        (FLASH_2_5, vec![prompt("a")], json!({"aspect_ratio": "1:8"})),
        (PRO, vec![prompt("a")], json!({"aspect_ratio": "8:1"})),
        (NANO_BANANA_2_1, vec![prompt("a")], json!({"aspect_ratio": "1:4"})),
        (FLASH_LITE, vec![prompt("a")], json!({"aspect_ratio": "4:1"})),
        (FLASH, vec![prompt("a")], json!({"aspect_ratio": "7:3"})),
        (FLASH, vec![prompt("a")], json!({"seed": 4})),
        ("imagen-4.0-generate-001", vec![prompt("a")], json!({})),
        ("../../other", vec![prompt("a")], json!({})),
    ];
    for (n, (model, inputs, params)) in cases.into_iter().enumerate() {
        let t = Transport::new(vec![granted(TOKEN), made()]);
        let scope = crate::SubmitContext::new();
        let error = block_on(provider(&t).submit_with_delivery(&job(model, inputs, params), &scope)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput, "case {n}: {}", error.message);
        assert!(t.requests().is_empty(), "case {n}: nothing, not even a token request, is sent");
        assert!(scope.finish_error().is_some(), "case {n}: nothing was sent, so it was not accepted");
    }
    let mut total = vec![prompt("a")];
    let big = {
        let mut b = png();
        b.resize(6_000_000, 0);
        b
    };
    total.extend((0..9).map(|_| media("image/png", big.clone())));
    let t = Transport::new(vec![]);
    assert_eq!(block_on(provider(&t).submit(&job(FLASH, total, json!({})))).unwrap_err().kind, ErrorKind::UnsupportedInput, "48 MiB of references at most");
    assert!(t.requests().is_empty());
}

#[test]
fn a_blocked_prompt_or_filtered_image_is_a_clear_blocked_content_error() {
    let error = failed(
        json!({"promptFeedback": {"blockReason": "PROHIBITED_CONTENT", "blockReasonMessage": "The prompt was blocked."}, "usageMetadata": {"promptTokenCount": 8}}),
    );
    assert_eq!(error.kind, ErrorKind::BlockedContent);
    assert!(error.message.contains("blocked the prompt (PROHIBITED_CONTENT): The prompt was blocked."), "{}", error.message);
    for reason in ["IMAGE_SAFETY", "SAFETY", "IMAGE_PROHIBITED_CONTENT", "IMAGE_RECITATION"] {
        let error = failed(
            json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "I can't make that."}]}, "finishReason": reason, "finishMessage": "Unable to show the generated image."}]}),
        );
        assert_eq!(error.kind, ErrorKind::BlockedContent, "{reason}");
        assert!(error.message.contains(&format!("filters stopped the image ({reason})")) && error.message.contains("Unable to show"), "{}", error.message);
    }
    let error = failed(json!({"candidates": [{"finishReason": "IMAGE_SAFETY"}]}));
    assert_eq!(error.kind, ErrorKind::BlockedContent, "a filtered candidate may carry no content at all");
    let error = failed(json!({"promptFeedback": {"blockReason": "x\u{1b}[31m"}}));
    assert!(error.message.contains("(unknown reason)"), "an odd code isn't echoed: {}", error.message);
}

#[test]
fn an_answer_without_an_image_fails_with_the_models_words() {
    let error = failed(json!({"candidates": [{"content": {"role": "model", "parts": [
        {"text": "secret plan", "thought": true}, {"text": "Which breed of cat would you like?"}]}, "finishReason": "STOP"}]}));
    assert_eq!(error.kind, ErrorKind::JobFailed);
    assert!(error.message.contains("without an image (STOP): Which breed") && !error.message.contains("secret plan"), "{}", error.message);
    let error = failed(json!({"candidates": [{"finishReason": "NO_IMAGE"}]}));
    assert_eq!(error.kind, ErrorKind::JobFailed);
    assert!(error.message.contains("NO_IMAGE"), "{}", error.message);
    let error = failed(json!({"candidates": [{"content": {"parts": [{"text": format!("leak Bearer {TOKEN}")}]}, "finishReason": "STOP"}]}));
    assert!(!error.message.contains(TOKEN) && error.message.contains("withheld"), "{}", error.message);
}

#[test]
fn several_images_all_come_back_and_usage_may_be_missing() {
    let t = Transport::new(vec![
        granted(TOKEN),
        ok(json!({"candidates": [{"content": {"parts": [
            {"inlineData": {"mimeType": "image/png", "data": b64(&png())}},
            {"text": "and another"},
            {"inlineData": {"mimeType": "image/jpeg", "data": b64(&jpeg())}}]}, "finishReason": "STOP"}]})),
    ]);
    let Submission::Finished(done) = block_on(provider(&t).submit(&job(FLASH, vec![prompt("two cats")], json!({})))).unwrap() else { panic!() };
    assert_eq!(done.outputs.len(), 2);
    assert_eq!(done.usage, crate::Usage::default(), "no tokens reported: the estimate is charged");
}

#[test]
fn limits_may_only_be_lowered() {
    let t = Transport::new(vec![]);
    let lower = Limits { response_bytes: 4096, output_bytes: 512, ..Limits::HARD };
    assert!(GoogleGeminiImages::with_limits(&stored(), t.clone(), fixed_clock(NOW), lower).is_ok());
    let error =
        GoogleGeminiImages::with_limits(&stored(), t, fixed_clock(NOW), Limits { image_bytes: Limits::HARD.image_bytes + 1, ..Limits::HARD }).err().unwrap();
    assert_eq!(error.kind, ErrorKind::UnsupportedInput);
}

#[test]
fn a_reply_that_is_not_an_image_or_too_large_is_malformed() {
    let with = |mime: &str, data: String| {
        ok(json!({"candidates": [{"content": {"parts": [{"inlineData": {"mimeType": mime, "data": data}}]}, "finishReason": "STOP"}]}))
    };
    let mut gif = b"GIF89a".to_vec();
    gif.resize(32, 0);
    let replies = [
        with("image/png", b64(&jpeg())),
        with("image/gif", b64(&gif)),
        with("image/png", b64(b"<svg></svg>")),
        with("image/png", String::new()),
        with("image/png", "not base64!".into()),
        ok(json!({"candidates": []})),
        ok(json!({"candidates": [{"content": {"parts": [{"inlineData": {"data": b64(&png())}}]}}]})),
        ok(json!({"candidates": "many"})),
        Ok(HttpResponse { status: 200, headers: vec![], body: b"<html>".to_vec() }),
    ];
    for (n, reply) in replies.into_iter().enumerate() {
        let t = Transport::new(vec![granted(TOKEN), reply]);
        let error = block_on(provider(&t).submit(&job(FLASH, vec![prompt("a")], json!({})))).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Malformed, "case {n}: {}", error.message);
    }
    let sized = |limits: Limits, reply| {
        let t = Transport::new(vec![granted(TOKEN), reply]);
        let small = GoogleGeminiImages::with_limits(&stored(), t, fixed_clock(NOW), limits).unwrap();
        block_on(small.submit(&job(FLASH, vec![prompt("a")], json!({})))).unwrap_err()
    };
    let wide = sized(Limits::HARD, with("image/png", b64(&sized_png(20_000, 1))));
    assert!(wide.kind == ErrorKind::Malformed && wide.message.contains("dimensions"), "{}", wide.message);
    let mut big = png();
    big.resize(900, 0);
    assert_eq!(sized(Limits { output_bytes: 512, ..Limits::HARD }, with("image/png", b64(&big))).kind, ErrorKind::Malformed, "one image too large");
    let two = ok(json!({"candidates": [{"content": {"parts": [
        {"inlineData": {"mimeType": "image/png", "data": b64(&big)}}, {"inlineData": {"mimeType": "image/png", "data": b64(&big)}}]}}]}));
    let error = sized(Limits { total_output_bytes: 1024, ..Limits::HARD }, two);
    assert!(error.kind == ErrorKind::Malformed && error.message.contains("together"), "{}", error.message);
    let error = sized(Limits { response_bytes: 64, ..Limits::HARD }, with("image/png", b64(&big)));
    assert!(error.kind == ErrorKind::Malformed && error.message.contains("byte limit"), "{}", error.message);
}

#[test]
fn errors_map_to_shared_kinds_and_only_canonical_refusals_clear_the_charge() {
    let google = |code: u16, status: &str, message: &str| serde_json::to_vec(&json!({"error": {"code": code, "status": status, "message": message}})).unwrap();
    let cases: Vec<(u16, Vec<u8>, ErrorKind, bool)> = vec![
        (400, google(400, "INVALID_ARGUMENT", "Unable to process input image."), ErrorKind::UnsupportedInput, true),
        (401, google(401, "UNAUTHENTICATED", "Request had invalid authentication credentials."), ErrorKind::Auth, true),
        (403, google(403, "PERMISSION_DENIED", "Vertex AI API has not been used in project"), ErrorKind::Auth, true),
        (404, google(404, "NOT_FOUND", "Publisher Model was not found."), ErrorKind::NotFound, true),
        (429, google(429, "RESOURCE_EXHAUSTED", "Quota exceeded."), ErrorKind::RateLimited, true),
        // Another shape or status stays possibly billed.
        (400, google(400, "FAILED_PRECONDITION", "odd"), ErrorKind::UnsupportedInput, false),
        (400, b"bad".to_vec(), ErrorKind::UnsupportedInput, false),
        (429, b"{}".to_vec(), ErrorKind::RateLimited, false),
        (500, google(500, "INTERNAL", "Internal error encountered."), ErrorKind::Server, false),
        (503, google(503, "UNAVAILABLE", "The service is currently unavailable."), ErrorKind::Busy, false),
        (504, b"".to_vec(), ErrorKind::Timeout, false),
        (302, b"".to_vec(), ErrorKind::Malformed, false),
    ];
    for (status, reply_body, kind, refusal) in cases {
        let t = Transport::new(vec![granted(TOKEN), Ok(HttpResponse { status, headers: vec![], body: reply_body.clone() })]);
        let scope = crate::SubmitContext::new();
        let error = block_on(provider(&t).submit_with_delivery(&job(FLASH, vec![prompt("a")], json!({})), &scope)).unwrap_err();
        let shown = String::from_utf8_lossy(&reply_body);
        assert_eq!(error.kind, kind, "HTTP {status} {shown}");
        assert!(error.message.contains(&format!("HTTP {status}")), "{}", error.message);
        assert_eq!(scope.finish_error().is_some(), refusal, "HTTP {status} {shown}: refusal evidence");
    }
    let t = Transport::new(vec![
        granted(TOKEN),
        Ok(HttpResponse { status: 429, headers: vec![("Retry-After".into(), "30".into())], body: google(429, "RESOURCE_EXHAUSTED", "Quota exceeded.") }),
    ]);
    let error = block_on(provider(&t).submit(&job(FLASH, vec![prompt("a")], json!({})))).unwrap_err();
    assert_eq!(error.retry_after_ms, Some(30_000));
    let t = Transport::new(vec![
        granted(TOKEN),
        Ok(HttpResponse { status: 400, headers: vec![], body: google(400, "INVALID_ARGUMENT", "Unable to process input image.") }),
    ]);
    let error = block_on(provider(&t).submit(&job(FLASH, vec![prompt("a")], json!({})))).unwrap_err();
    assert!(error.message.contains("Unable to process input image."), "Google's explanation of a bad request is shown: {}", error.message);
    let t = Transport::new(vec![granted(TOKEN), Ok(HttpResponse { status: 403, headers: vec![], body: google(403, "PERMISSION_DENIED", "denied") })]);
    let error = block_on(provider(&t).submit(&job(FLASH, vec![prompt("a")], json!({})))).unwrap_err();
    assert!(error.message.contains(PROJECT) && error.message.contains("Vertex AI API"), "{}", error.message);
}

#[test]
fn a_refused_token_is_dropped_so_the_next_run_asks_for_a_fresh_one() {
    let unauthenticated = serde_json::to_vec(&json!({"error": {"code": 401, "status": "UNAUTHENTICATED", "message": "expired"}})).unwrap();
    let t = Transport::new(vec![granted(TOKEN), Ok(HttpResponse { status: 401, headers: vec![], body: unauthenticated }), granted("ya29.fresh"), made()]);
    let p = provider(&t);
    assert_eq!(block_on(p.submit(&job(FLASH, vec![prompt("a")], json!({})))).unwrap_err().kind, ErrorKind::Auth);
    assert!(block_on(p.submit(&job(FLASH, vec![prompt("a")], json!({})))).is_ok());
    let requests = t.requests();
    assert_eq!(requests.len(), 4);
    assert!(requests[3].1.headers.iter().any(|(n, v)| n == "Authorization" && v == "Bearer ya29.fresh"));
}

#[test]
fn a_lost_reply_is_possibly_billed_and_never_retried_and_the_token_never_appears_in_an_error() {
    let t = Transport::new(vec![granted(TOKEN), Err(ProviderError::new(ErrorKind::Network, format!("connection reset; Bearer {TOKEN}")))]);
    let p = provider(&t);
    assert!(!p.retry_ambiguous_submit());
    let scope = crate::SubmitContext::new();
    let error = block_on(p.submit_with_delivery(&job(FLASH, vec![prompt("a")], json!({})), &scope)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.message.contains("may have been made and billed") && !error.message.contains(TOKEN), "{}", error.message);
    assert!(scope.finish_error().is_none(), "the request went out");
    let echo = serde_json::to_vec(&json!({"error": {"code": 400, "status": "INVALID_ARGUMENT", "message": format!("bad header Bearer {TOKEN}")}})).unwrap();
    let t = Transport::new(vec![granted(TOKEN), Ok(HttpResponse { status: 400, headers: vec![], body: echo })]);
    let error = block_on(provider(&t).submit(&job(FLASH, vec![prompt("a")], json!({})))).unwrap_err();
    assert!(!error.message.contains(TOKEN) && error.message.contains("withheld"), "{}", error.message);
}

#[test]
fn a_refused_token_exchange_sends_no_image_request() {
    let t = Transport::new(vec![Ok(HttpResponse { status: 400, headers: vec![], body: br#"{"error":"invalid_grant"}"#.to_vec() })]);
    let scope = crate::SubmitContext::new();
    assert!(block_on(provider(&t).submit_with_delivery(&job(FLASH, vec![prompt("a")], json!({})), &scope)).is_err());
    assert_eq!(t.requests().len(), 1, "only the free token request went out");
    assert!(scope.finish_error().is_some(), "nothing paid was sent");
}

#[test]
fn polling_and_cancelling_are_unsupported() {
    let t = Transport::new(vec![]);
    let p = provider(&t);
    let handle = JobHandle { remote_id: "x".into(), data: json!({}) };
    assert_eq!(block_on(p.poll(&handle)).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(block_on(p.cancel(&handle)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert_eq!(p.id(), "google-gemini-images");
}
