use super::*;
use crate::{HttpMethod, HttpResponse, JobInput, Output};
use futures::executor::block_on;
use serde_json::json;
use std::{collections::VecDeque, sync::Mutex};

const KEY: &str = "fixture-stability-key-not-real";
const ID: &str = "a6dc6c6e20acda010fe14d71f180658f2896ed9b4ec25aa99a6ff06c796987c4";
const GLB: &[u8] = include_bytes!("../fal/fixtures/triangle.glb");
struct Wire {
    replies: Mutex<VecDeque<Result<HttpResponse, ProviderError>>>,
    requests: Mutex<Vec<HttpRequest>>,
}
impl Wire {
    fn new(replies: impl IntoIterator<Item = HttpResponse>) -> Arc<Self> {
        Arc::new(Self { replies: Mutex::new(replies.into_iter().map(Ok).collect()), requests: Mutex::default() })
    }
}
impl Http for Wire {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-capable path")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.replies.lock().unwrap().pop_front().expect("unexpected HTTP");
        Box::pin(async move { reply })
    }
}
fn provider(wire: Arc<Wire>) -> Stability {
    Stability::new(Secret::new(KEY).unwrap(), wire)
}
fn png(side: u32) -> Vec<u8> {
    nodecraft_media::encode_png(side, side, &vec![180; usize::try_from(side * side * 4).unwrap()], &Default::default()).unwrap()
}
pub(super) fn wav() -> Vec<u8> {
    let data = vec![0u8; 441 * 4];
    let mut out = b"RIFF".to_vec();
    out.extend(u32::try_from(data.len() + 36).unwrap().to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(44_100u32.to_le_bytes());
    out.extend(176_400u32.to_le_bytes());
    out.extend(4u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    out.extend(data);
    out
}
fn job(model: &str) -> ProviderJob {
    ProviderJob {
        model: model.into(),
        params: Default::default(),
        inputs: if model == FAST_3D {
            vec![JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: png(64).into() }]
        } else {
            vec![JobInput::Text { role: "prompt".into(), text: "A blue paper boat".into() }]
        },
        idempotency_key: "fixture-36".into(),
    }
}
fn reply(status: u16, body: Vec<u8>, mime: &str) -> HttpResponse {
    HttpResponse { status, headers: vec![("Content-Type".into(), mime.into())], body }
}
fn accepted() -> HttpResponse {
    reply(202, serde_json::to_vec(&json!({"id":ID})).unwrap(), "application/json")
}
fn value<'a>(request: &'a HttpRequest, name: &str) -> &'a str {
    let text = std::str::from_utf8(&request.body).unwrap();
    let marker = format!("name=\"{name}\"\r\n\r\n");
    text.split_once(&marker).unwrap().1.split_once("\r\n").unwrap().0
}

#[test]
fn stability36_core_and_mesh_finish_as_viewable_typed_media_with_private_fixed_origin_requests() {
    for (model, mime, bytes) in [(CORE, "image/png", png(4)), (FAST_3D, "model/gltf-binary", GLB.to_vec())] {
        let http = Wire::new([reply(200, bytes.clone(), mime)]);
        let p = provider(http.clone());
        let Submission::Finished(done) = block_on(p.submit(&job(model))).unwrap() else { panic!("synchronous") };
        assert!(matches!(done.outputs.as_slice(),[Output::Bytes {mime:actual,bytes:actual_bytes}] if actual==mime && actual_bytes.as_ref()==bytes));
        assert_eq!(done.usage, crate::Usage::default());
        assert!(!p.retry_ambiguous_submit());
        let requests = http.requests.lock().unwrap();
        let request = &requests[0];
        assert_eq!(request.url, format!("{API}/{}", Model::read(model).unwrap().path()));
        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(request.secret_headers, ["authorization"]);
        assert!(request.headers.iter().any(|(name, value)| name == "Authorization" && value == &format!("Bearer {KEY}")));
        assert!(request.headers.iter().any(|(name, value)| name == "User-Agent" && value == crate::USER_AGENT));
        assert!(!format!("{request:?}").contains(KEY));
        assert!(!request.url.contains("fixture-36"));
        if model == CORE {
            assert_eq!(value(request, "prompt"), "A blue paper boat");
            assert_eq!(value(request, "output_format"), "png");
            assert_eq!(request.max_response_bytes, IMAGE_BYTES);
        } else {
            let input = png(64);
            assert!(request.body.windows(input.len()).any(|bytes| bytes == input));
            assert_eq!(request.max_response_bytes, MESH_BYTES);
        }
    }
}

#[test]
fn stability36_audio_accepts_persists_and_polls_the_original_handle_without_another_paid_post() {
    let pending = reply(202, serde_json::to_vec(&json!({"id":ID,"status":"in-progress"})).unwrap(), "application/json");
    let http = Wire::new([accepted(), pending, reply(200, wav(), "audio/wav")]);
    let p = provider(http.clone());
    let mut j = job(AUDIO);
    j.params = json!({"duration_seconds":380,"seed":4294967294u64,"steps":4,"cfg_scale":25}).as_object().unwrap().clone();
    let Submission::Queued(handle) = block_on(p.submit(&j)).unwrap() else { panic!("queued") };
    let persisted: JobHandle = serde_json::from_slice(&serde_json::to_vec(&handle).unwrap()).unwrap();
    assert!(matches!(block_on(p.poll(&persisted)).unwrap(), JobProgress::Running { .. }));
    assert!(
        matches!(block_on(p.poll(&persisted)).unwrap(),JobProgress::Done(done) if matches!(done.outputs.as_slice(),[Output::Bytes {mime,..}] if mime=="audio/wav"))
    );
    assert!(block_on(p.cancel(&persisted)).unwrap_err().message.contains("may still run and be billed"));
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests.iter().filter(|r| r.method == HttpMethod::Post).count(), 1);
    assert_eq!(requests[0].max_response_bytes, METADATA);
    assert_eq!(value(&requests[0], "duration"), "380");
    assert_eq!(value(&requests[0], "seed"), "4294967294");
    assert_eq!(value(&requests[0], "output_format"), "wav");
    for request in requests.iter().skip(1) {
        assert_eq!(request.url, format!("{API}/audio/results/{ID}"));
        assert_eq!(request.max_response_bytes, AUDIO_BYTES);
        assert_eq!(request.secret_headers, ["authorization"]);
    }
}

#[test]
fn stability36_all_invalid_requests_refuse_before_http_with_positive_nonbilling_evidence() {
    let mut cases = vec![];
    for model in [CORE, AUDIO, FAST_3D] {
        let mut j = job(model);
        j.params.insert("callback_url".into(), json!("https://private.example"));
        cases.push(j);
        let mut j = job(model);
        j.inputs.clear();
        cases.push(j);
        let mut j = job(model);
        j.inputs.push(JobInput::Text { role: "extra".into(), text: "x".into() });
        cases.push(j);
    }
    for (model, params) in [
        (CORE, json!({"seed":4294967295u64})),
        (CORE, json!({"output_format":"svg"})),
        (CORE, json!({"negative_prompt":"x".repeat(10001)})),
        (AUDIO, json!({"duration_seconds":0})),
        (AUDIO, json!({"duration_seconds":381})),
        (AUDIO, json!({"duration_seconds":1.5})),
        (AUDIO, json!({"steps":9})),
        (AUDIO, json!({"cfg_scale":26})),
        (FAST_3D, json!({"foreground_ratio":0})),
        (FAST_3D, json!({"texture_resolution":"4096"})),
        (FAST_3D, json!({"vertex_count":-1})),
        (FAST_3D, json!({"remesh":"voxel"})),
    ] {
        let mut j = job(model);
        j.params = params.as_object().unwrap().clone();
        cases.push(j);
    }
    let mut unknown = job(CORE);
    unknown.model = "https://other.example".into();
    cases.push(unknown);
    for model in [CORE, AUDIO] {
        for text in [String::new(), "x".repeat(10001)] {
            let mut j = job(model);
            j.inputs = vec![JobInput::Text { role: "prompt".into(), text }];
            cases.push(j);
        }
    }
    for (mime, bytes) in [("image/png", png(63)), ("image/jpeg", png(64)), ("image/png", b"broken".to_vec()), ("image/png", vec![0; 4 * MIB + 1])] {
        let mut j = job(FAST_3D);
        j.inputs = vec![JobInput::Media { role: "image".into(), mime: mime.into(), bytes: bytes.into() }];
        cases.push(j);
    }
    let http = Wire::new([]);
    let p = provider(http.clone());
    for case in cases {
        let scope = SubmitContext::new();
        assert_eq!(block_on(p.submit_with_delivery(&case, &scope)).unwrap_err().kind, ErrorKind::UnsupportedInput);
        assert!(scope.finish_error().is_some());
    }
    assert!(http.requests.lock().unwrap().is_empty());
}

#[test]
fn stability36_typed_documented_refusals_clear_billing_but_ambiguous_statuses_do_not() {
    for (status, name, kind) in [
        (400, "bad_request", ErrorKind::UnsupportedInput),
        (401, "unauthorized", ErrorKind::Auth),
        (402, "payment_required", ErrorKind::Budget),
        (403, "content_moderation", ErrorKind::BlockedContent),
        (413, "payload_too_large", ErrorKind::UnsupportedInput),
        (422, "unprocessable_entity", ErrorKind::UnsupportedInput),
        (429, "rate_limit_exceeded", ErrorKind::RateLimited),
    ] {
        let mut response = reply(status, serde_json::to_vec(&json!({"id":"public-fixture","name":name,"errors":[KEY]})).unwrap(), "application/json");
        response.headers.push(("Retry-After".into(), "2".into()));
        let http = Wire::new([response]);
        let scope = SubmitContext::new();
        let error = block_on(provider(http).submit_with_delivery(&job(CORE), &scope)).unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(error.retry_after_ms, Some(2000));
        assert!(!error.message.contains(KEY));
        assert!(scope.finish_error().is_some(), "{status}");
    }
    for status in [302, 400, 401, 402, 403, 422, 429, 500, 503] {
        let http = Wire::new([reply(status, KEY.as_bytes().to_vec(), "text/html")]);
        let scope = SubmitContext::new();
        let error = block_on(provider(http).submit_with_delivery(&job(CORE), &scope)).unwrap_err();
        assert!(!error.message.contains(KEY));
        assert!(scope.finish_error().is_none(), "unparsed HTTP {status} cannot prove nonbilling");
    }
}

#[test]
fn stability36_lost_and_malformed_acceptance_never_claim_nonbilling_or_a_handle() {
    for body in [format!(r#"{{"id":"{ID}","id":"{ID}"}}"#), format!(r#"{{"id":"{ID}","error":"private"}}"#), "{\"id\":\"../private\"}".into()] {
        let http = Wire::new([reply(202, body.into_bytes(), "application/json")]);
        let scope = SubmitContext::new();
        assert!(block_on(provider(http).submit_with_delivery(&job(AUDIO), &scope)).is_err());
        assert!(scope.finish_error().is_none());
    }
    let http = Wire::new([]);
    http.replies.lock().unwrap().push_back(Err(ProviderError::new(ErrorKind::Network, KEY)));
    let scope = SubmitContext::new();
    let error = block_on(provider(http).submit_with_delivery(&job(AUDIO), &scope)).unwrap_err();
    assert!(!error.message.contains(KEY));
    assert!(scope.finish_error().is_none());
}

#[test]
fn stability36_tampered_handles_and_conflicting_pending_ids_do_not_pick_another_job_or_host() {
    let http = Wire::new([]);
    let p = provider(http.clone());
    for value in [
        JobHandle { remote_id: "https://private.example".into(), data: json!({"v":1,"duration":30}) },
        JobHandle { remote_id: ID.into(), data: json!({"v":2,"duration":30}) },
        JobHandle { remote_id: ID.into(), data: json!({"v":1,"duration":381}) },
        JobHandle { remote_id: ID.into(), data: json!({"v":1,"duration":30,"url":"https://private.example"}) },
    ] {
        assert!(block_on(p.poll(&value)).is_err());
        assert!(block_on(p.cancel(&value)).is_err());
    }
    assert!(http.requests.lock().unwrap().is_empty());
    let http = Wire::new([reply(202, serde_json::to_vec(&json!({"id":"b".repeat(64),"status":"in-progress"})).unwrap(), "application/json")]);
    assert!(block_on(provider(http).poll(&JobHandle { remote_id: ID.into(), data: json!({"v":1,"duration":30}) })).is_err());
}

#[test]
fn stability36_bad_media_metadata_moderation_and_truncation_are_not_recorded() {
    let mut cases = vec![
        (Model::Core, reply(200, b"<html>error</html>".to_vec(), "image/png")),
        (Model::Mesh, reply(200, b"glTFbroken".to_vec(), "model/gltf-binary")),
        (Model::Audio, reply(200, wav(), "audio/mpeg")),
    ];
    let mut truncated = wav();
    truncated.pop();
    cases.push((Model::Audio, reply(200, truncated, "audio/wav")));
    let mut filtered = reply(200, png(4), "image/png");
    filtered.headers.push(("finish-reason".into(), "CONTENT_FILTERED".into()));
    cases.push((Model::Core, filtered));
    let mut wrong_length = reply(200, png(4), "image/png");
    wrong_length.headers.push(("Content-Length".into(), "1".into()));
    assert!(response::bounds(&wrong_length, IMAGE_BYTES).is_err());
    let mut duplicate = reply(200, png(4), "image/png");
    duplicate.headers.push(("content-type".into(), "image/png".into()));
    cases.push((Model::Core, duplicate));
    for (model, reply) in cases {
        assert!(response::media(reply, model, Some(30)).is_err());
    }
}

#[test]
fn stability36_fixed_multipart_boundary_cannot_be_injected_through_prompt_text() {
    let normal = request::prepare(&job(CORE)).unwrap().request;
    let boundary = normal.headers.iter().find(|(name, _)| name == "Content-Type").unwrap().1.split("boundary=").nth(1).unwrap();
    let mut j = job(CORE);
    j.inputs = vec![JobInput::Text {
        role: "prompt".into(),
        text: format!("innocent\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"output_format\"\r\n\r\nsvg"),
    }];
    assert!(request::prepare(&j).is_err());
}
