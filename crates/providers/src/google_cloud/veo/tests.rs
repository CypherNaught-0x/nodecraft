use super::super::tests::{NOW, PROJECT, Transport, fixed_clock, granted, ok, stored};
use super::*;
use crate::{HttpMethod, JobInput};
use base64::Engine as _;
use futures::executor::block_on;
use serde_json::{Value, json};

const OPERATION: &str = "a1b07c8e-7b5a-4aba-bb34-3e1ccb8afcc8";
const TOKEN: &str = "ya29.fixture-token";

fn name(project: &str, model: &str) -> String {
    format!("projects/{project}/locations/us-central1/publishers/google/models/{model}/operations/{OPERATION}")
}
fn mp4() -> Vec<u8> {
    let mut bytes = b"\0\0\0\x20ftypisom\0\0\x02\0isomiso2avc1mp41".to_vec();
    bytes.resize(64, 0);
    bytes
}
fn png() -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[120; 16], &nodecraft_media::Limits::default()).unwrap()
}
fn job(model: &str, inputs: Vec<JobInput>) -> ProviderJob {
    ProviderJob { model: model.into(), params: Default::default(), inputs, idempotency_key: "fixture-operation".into() }
}
fn prompt(text: &str) -> JobInput {
    JobInput::Text { role: "prompt".into(), text: text.into() }
}
fn image(role: &str) -> JobInput {
    JobInput::Media { role: role.into(), mime: "image/png".into(), bytes: Arc::from(png()) }
}
fn started(model: &str) -> Result<HttpResponse, ProviderError> {
    ok(json!({"name": name("123456789012", model)}))
}
fn done(video: Value) -> Result<HttpResponse, ProviderError> {
    ok(json!({"name": name("123456789012", GENERATE), "done": true, "response": {
        "@type": "type.googleapis.com/cloud.ai.large_models.vision.GenerateVideoResponse",
        "raiMediaFilteredCount": 0, "videos": [video]}}))
}
fn inline() -> Value {
    json!({"bytesBase64Encoded": base64::engine::general_purpose::STANDARD.encode(mp4()), "mimeType": "video/mp4"})
}
fn provider(t: &Arc<Transport>) -> Veo {
    Veo::new(&stored(), t.clone(), fixed_clock(NOW)).unwrap()
}
fn body(r: &HttpRequest) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}
fn endpoint(model: &str, method: &str) -> String {
    format!("https://us-central1-aiplatform.googleapis.com/v1/projects/{PROJECT}/locations/us-central1/publishers/google/models/{model}:{method}")
}

#[test]
fn text_to_video_starts_an_operation_then_fetches_it_until_the_video_is_inline() {
    let t = Transport::new(vec![granted(TOKEN), started(GENERATE), ok(json!({"name": name("123456789012", GENERATE)})), done(inline())]);
    let p = provider(&t);
    let mut j = job(GENERATE, vec![prompt("A marble rolling on a track")]);
    j.params.insert("aspect_ratio".into(), json!("9:16"));
    j.params.insert("resolution".into(), json!("4k"));
    j.params.insert("duration_seconds".into(), json!("6"));
    j.params.insert("seed".into(), json!("42"));
    let Submission::Queued(handle) = block_on(p.submit(&j)).unwrap() else { panic!("expected an operation") };
    assert_eq!(handle.remote_id, name("123456789012", GENERATE), "a project number in the name is fine");
    assert!(matches!(block_on(p.poll(&handle)).unwrap(), JobProgress::Running { .. }));
    let JobProgress::Done(done) = block_on(p.poll(&handle)).unwrap() else { panic!("expected the video") };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: MP4.into(), bytes: Arc::from(mp4()) }]);

    let requests = t.requests();
    assert_eq!(requests.len(), 4, "one token serves the start and both fetches");
    let (redirects, start) = &requests[1];
    assert!(!redirects);
    assert_eq!((start.method, start.url.as_str()), (HttpMethod::Post, endpoint(GENERATE, "predictLongRunning").as_str()));
    assert_eq!(
        body(start),
        json!({"instances": [{"prompt": "A marble rolling on a track"}],
            "parameters": {"sampleCount": 1, "durationSeconds": 6, "generateAudio": true, "aspectRatio": "9:16", "resolution": "4k", "seed": 42}})
    );
    let fetch = &requests[2].1;
    assert_eq!((fetch.method, fetch.url.as_str()), (HttpMethod::Post, endpoint(GENERATE, "fetchPredictOperation").as_str()));
    assert_eq!(body(fetch), json!({"operationName": name("123456789012", GENERATE)}));
    for (redirects, r) in &requests[1..] {
        assert!(!redirects);
        assert!(r.headers.iter().any(|(n, v)| n == "Authorization" && v == &format!("Bearer {TOKEN}")));
        assert!(r.secret_headers.iter().any(|n| n == "authorization"), "the token travels only as a secret header");
        assert!(r.headers.iter().any(|(n, v)| n == "User-Agent" && v == crate::USER_AGENT));
        assert!(!r.url.contains(TOKEN));
    }
}

#[test]
fn frames_and_references_are_sent_inline_as_veo_reads_them() {
    let data = base64::engine::general_purpose::STANDARD.encode(png());
    let picture = json!({"mimeType": "image/png", "bytesBase64Encoded": data});
    let t = Transport::new(vec![granted(TOKEN), started(FAST)]);
    block_on(provider(&t).submit(&job(FAST, vec![image("last_frame"), prompt("forest to snow"), image("first_frame")]))).unwrap();
    let instance = body(&t.requests()[1].1)["instances"][0].clone();
    assert_eq!(instance, json!({"prompt": "forest to snow", "image": picture, "lastFrame": picture}));

    let t = Transport::new(vec![granted(TOKEN), started(GENERATE)]);
    block_on(provider(&t).submit(&job(GENERATE, vec![prompt("the cat"), image("reference"), image("reference")]))).unwrap();
    let sent = body(&t.requests()[1].1);
    let reference = json!({"image": picture, "referenceType": "asset"});
    assert_eq!(sent["instances"][0], json!({"prompt": "the cat", "referenceImages": [reference, reference]}));
    assert_eq!(sent["parameters"]["durationSeconds"], json!(8));
}

#[test]
fn jobs_veo_cant_make_are_refused_before_anything_is_sent() {
    let refused = |model: &str, inputs: Vec<JobInput>, params: Value| {
        let mut j = job(model, inputs);
        j.params = params.as_object().unwrap().clone();
        let t = Transport::new(vec![]);
        let error = block_on(provider(&t).submit(&j)).unwrap_err();
        assert!(t.requests().is_empty(), "nothing sent, not even for a token: {error:?}");
        assert_eq!(error.kind, ErrorKind::UnsupportedInput, "{error:?}");
        error.message
    };
    let p = || vec![prompt("a")];
    assert!(refused("veo-3.0-generate-001", p(), json!({})).contains("Veo 3.1"));
    assert!(refused(GENERATE, vec![], json!({})).contains("needs a prompt"));
    assert!(refused(GENERATE, vec![prompt(" ")], json!({})).contains("characters"));
    assert!(refused(GENERATE, vec![prompt(&"x".repeat(8_001))], json!({})).contains("characters"));
    assert!(refused(GENERATE, p(), json!({"duration_seconds": 5})).contains("4, 6 or 8"));
    assert!(refused(GENERATE, p(), json!({"duration_seconds": "8s"})).contains("4, 6 or 8"));
    assert!(refused(FAST, p(), json!({"resolution": "4k"})).contains("resolution"));
    assert!(refused(GENERATE, p(), json!({"aspect_ratio": "1:1"})).contains("aspect_ratio"));
    assert!(refused(GENERATE, p(), json!({"seed": 4_294_967_296u64})).contains("seed"));
    assert!(refused(GENERATE, p(), json!({"storage_uri": "gs://x"})).contains("storage_uri"));
    assert!(refused(LITE, vec![prompt("a"), image("first_frame")], json!({})).contains("Lite"));
    assert!(refused(GENERATE, vec![prompt("a"), image("last_frame")], json!({})).contains("first frame"));
    assert!(refused(GENERATE, vec![prompt("a"), image("first_frame"), image("reference")], json!({})).contains("not both"));
    assert!(refused(GENERATE, vec![prompt("a"), image("reference")], json!({"duration_seconds": 4})).contains("8 seconds"));
    let four = vec![prompt("a"), image("reference"), image("reference"), image("reference"), image("reference")];
    assert!(refused(GENERATE, four, json!({})).contains("three"));
    let gif = JobInput::Media { role: "first_frame".into(), mime: "image/gif".into(), bytes: Arc::from(png()) };
    assert!(refused(GENERATE, vec![prompt("a"), gif], json!({})).contains("PNG or JPEG"));
    let lying = JobInput::Media { role: "first_frame".into(), mime: "image/jpeg".into(), bytes: Arc::from(png()) };
    assert!(refused(GENERATE, vec![prompt("a"), lying], json!({})).contains("don't match"));
}

#[test]
fn delivery_turns_unknown_only_once_the_token_is_in_hand() {
    // The token request failing leaves nothing started.
    let t = Transport::new(vec![Err(ProviderError::new(ErrorKind::Network, "reset"))]);
    let scope = crate::SubmitContext::new();
    assert!(block_on(provider(&t).submit_with_delivery(&job(GENERATE, vec![prompt("a")]), &scope)).is_err());
    assert!(scope.finish_error().is_some(), "no video was started");
    // The start request failing may hide a started video.
    let t = Transport::new(vec![granted(TOKEN), Err(ProviderError::new(ErrorKind::Network, "reset"))]);
    let scope = crate::SubmitContext::new();
    let error = block_on(provider(&t).submit_with_delivery(&job(GENERATE, vec![prompt("a")]), &scope)).unwrap_err();
    assert!(error.message.contains("may have been started and billed"), "{}", error.message);
    assert!(scope.finish_error().is_none(), "the video may exist");
    assert!(!provider(&t).retry_ambiguous_submit());
}

#[test]
fn http_refusals_say_what_to_fix_without_quoting_the_reply() {
    let reply =
        |status: u16| Ok(HttpResponse { status, headers: vec![("Retry-After".into(), "7".into())], body: br#"{"error":{"message":"secret echo"}}"#.to_vec() });
    for (status, kind, words) in [
        (403, ErrorKind::Auth, "Vertex AI User role"),
        (404, ErrorKind::NotFound, "no such Veo model"),
        (429, ErrorKind::RateLimited, "quota"),
        (400, ErrorKind::UnsupportedInput, "may have been billed"),
        (500, ErrorKind::Server, "may have been billed"),
        (302, ErrorKind::Malformed, "may have been billed"),
    ] {
        let t = Transport::new(vec![granted(TOKEN), reply(status)]);
        let error = block_on(provider(&t).submit(&job(GENERATE, vec![prompt("a")]))).unwrap_err();
        assert_eq!(error.kind, kind, "{status}");
        assert!(error.message.contains(words) && error.message.contains(PROJECT) == matches!(status, 403 | 404), "{}", error.message);
        assert!(!error.message.contains("secret echo"));
        assert_eq!(error.retry_after_ms, Some(7_000));
    }
    // A refused token is dropped, so the next request asks for a new one.
    let handle = JobHandle { remote_id: name(PROJECT, FAST), data: json!({"v": 1}) };
    let t = Transport::new(vec![granted(TOKEN), reply(401), granted("ya29.fresh"), done(inline())]);
    let p = provider(&t);
    assert_eq!(block_on(p.poll(&handle)).unwrap_err().kind, ErrorKind::Auth);
    assert!(matches!(block_on(p.poll(&handle)).unwrap(), JobProgress::Done(_)));
    let requests = t.requests();
    assert_eq!(requests[2].1.url, super::super::TOKEN_URI);
    assert!(requests[3].1.headers.iter().any(|(n, v)| n == "Authorization" && v == "Bearer ya29.fresh"));
    assert_eq!(requests[3].1.url, endpoint(FAST, "fetchPredictOperation"));
}

#[test]
fn operations_from_elsewhere_are_refused() {
    let t = Transport::new(vec![]);
    let p = provider(&t);
    for remote_id in [
        String::new(),
        OPERATION.to_owned(),
        name(PROJECT, "veo-3.0-generate-001"),
        name("../x", GENERATE),
        name(PROJECT, GENERATE).replace("us-central1", "europe-west4"),
        format!("{}/extra", name(PROJECT, GENERATE)),
        name(PROJECT, GENERATE).replace(OPERATION, "a?b"),
        format!("https://evil.example/{}", name(PROJECT, GENERATE)),
    ] {
        let handle = JobHandle { remote_id: remote_id.clone(), data: json!({"v": 1}) };
        assert_eq!(block_on(p.poll(&handle)).unwrap_err().kind, ErrorKind::Malformed, "{remote_id}");
    }
    let handle = JobHandle { remote_id: name(PROJECT, GENERATE), data: json!({"v": 2}) };
    assert!(block_on(p.poll(&handle)).is_err());
    assert!(t.requests().is_empty());
    // A start reply naming another model's operation isn't kept.
    let t = Transport::new(vec![granted(TOKEN), started(FAST)]);
    let error = block_on(provider(&t).submit(&job(GENERATE, vec![prompt("a")]))).unwrap_err();
    assert!(error.message.contains("may have been billed"), "{}", error.message);
    assert!(block_on(provider(&t).cancel(&JobHandle { remote_id: name(PROJECT, GENERATE), data: json!({"v": 1}) })).is_err());
}

#[test]
fn finished_operations_report_safety_blocks_failures_and_odd_videos() {
    let handle = JobHandle { remote_id: name(PROJECT, GENERATE), data: json!({"v": 1}) };
    let poll = |reply: Result<HttpResponse, ProviderError>| block_on(provider(&Transport::new(vec![granted(TOKEN), reply])).poll(&handle));
    let failed = |reply| match poll(reply) {
        Ok(JobProgress::Failed(error)) => error,
        other => panic!("expected a failed job, got {other:?}"),
    };
    let blocked = failed(ok(json!({"done": true, "response": {"raiMediaFilteredCount": 1,
        "raiMediaFilteredReasons": ["Veo could not generate videos because the input image violates Agent Platform's usage guidelines. Support codes: 15236754, 90789179"]}})));
    assert_eq!(blocked.kind, ErrorKind::BlockedContent);
    assert!(blocked.message.contains("(a celebrity's likeness, sexual content)"), "{}", blocked.message);
    assert!(!blocked.message.contains("input image"), "Google's text isn't quoted");
    let error = failed(ok(json!({"done": true, "error": {"code": 3, "message": "The prompt violates the usage guidelines. Support codes: 61493863"}})));
    assert_eq!((error.kind, error.message.contains("violence")), (ErrorKind::BlockedContent, true));
    let error = failed(ok(json!({"done": true, "error": {"code": 13, "message": "Internal error with private detail"}})));
    assert_eq!(error.kind, ErrorKind::JobFailed);
    assert!(error.message.contains("code 13") && !error.message.contains("private"), "{}", error.message);
    let odd = |reply| poll(reply).unwrap_err().kind;
    assert_eq!(odd(done(json!({"gcsUri": "gs://bucket/sample_0.mp4", "mimeType": "video/mp4"}))), ErrorKind::Malformed);
    assert_eq!(odd(done(json!({"bytesBase64Encoded": "!!", "mimeType": "video/mp4"}))), ErrorKind::Malformed);
    assert_eq!(odd(done(json!({"bytesBase64Encoded": base64::engine::general_purpose::STANDARD.encode(png())}))), ErrorKind::Malformed);
    assert_eq!(odd(done(json!({"bytesBase64Encoded": "AAAA", "mimeType": "video/webm"}))), ErrorKind::Malformed);
    assert_eq!(odd(ok(json!({"done": true, "response": {"videos": [inline(), inline()]}}))), ErrorKind::Malformed);
    assert_eq!(odd(ok(json!({"done": true, "response": {"videos": []}}))), ErrorKind::Malformed);
    assert_eq!(odd(ok(json!({"done": true}))), ErrorKind::Malformed);
    assert_eq!(odd(ok(json!("not an operation"))), ErrorKind::Malformed);
    assert!(matches!(poll(ok(json!({}))).unwrap(), JobProgress::Running { .. }), "an operation without `done` is still running");
    // A video over the limit is refused unread.
    let small = Limits { output_bytes: 32, ..Limits::HARD };
    let p = Veo::with_limits(&stored(), Transport::new(vec![granted(TOKEN), done(inline())]), fixed_clock(NOW), small).unwrap();
    assert_eq!(block_on(p.poll(&handle)).unwrap_err().kind, ErrorKind::Malformed);
    assert!(Veo::with_limits(&stored(), Transport::new(vec![]), fixed_clock(NOW), Limits { prompt_chars: 9_000, ..Limits::HARD }).is_err());
}

#[test]
fn a_damaged_stored_account_is_an_auth_error() {
    let secret = crate::secrets::Secret::new("gsa1:not:a:real:account:value").unwrap();
    assert_eq!(Veo::new(&secret, Transport::new(vec![]), fixed_clock(NOW)).err().map(|e| e.kind), Some(ErrorKind::Auth));
}
