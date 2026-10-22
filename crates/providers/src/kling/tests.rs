use super::*;
use crate::{HttpMethod, JobInput, SubmitContext};
use base64::Engine as _;
use futures::executor::block_on;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{collections::VecDeque, sync::Mutex};

const KEY: &str = "kling-fixture-api-key-not-real";
const ACCESS: &str = "fixture-access-key";
const SECRET: &str = "fixture-secret-key-not-real";
const TASK: &str = "860120339150405712";
const NOW: u64 = 1_790_000_000;

/// The fixture transport: replies in order, and which of the two send paths each request took.
#[derive(Default)]
struct Transport {
    requests: Mutex<Vec<(bool, HttpRequest)>>,
    replies: Mutex<VecDeque<Result<HttpResponse, ProviderError>>>,
}
impl Transport {
    fn new(replies: Vec<Result<HttpResponse, ProviderError>>) -> Arc<Self> {
        Arc::new(Self { requests: Mutex::new(vec![]), replies: Mutex::new(replies.into()) })
    }
    fn requests(&self) -> Vec<(bool, HttpRequest)> {
        self.requests.lock().unwrap().clone()
    }
    fn reply(&self, redirects: bool, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push((redirects, r));
        let reply = self.replies.lock().unwrap().pop_front().expect("fixture reply");
        Box::pin(async { reply })
    }
}
impl Http for Transport {
    fn send(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.reply(true, r)
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.reply(false, r)
    }
}
fn ok(v: Value) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&v).unwrap() })
}
fn refused(status: u16, code: u64, message: &str) -> Result<HttpResponse, ProviderError> {
    let body = serde_json::to_vec(&json!({"code": code, "message": message, "request_id": "r1"})).unwrap();
    Ok(HttpResponse { status, headers: vec![("Retry-After".into(), "7".into())], body })
}
fn bytes(body: Vec<u8>) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body })
}
fn envelope(data: Value) -> Result<HttpResponse, ProviderError> {
    ok(json!({"code": 0, "message": "SUCCEED", "request_id": "r1", "data": data}))
}
fn receipt() -> Result<HttpResponse, ProviderError> {
    envelope(json!({"task_id": TASK, "task_status": "submitted", "created_at": 1, "updated_at": 1}))
}
fn state(status: &str) -> Result<HttpResponse, ProviderError> {
    envelope(json!({"task_id": TASK, "task_status": status, "task_status_msg": ""}))
}
fn finished(list: &str) -> Result<HttpResponse, ProviderError> {
    envelope(json!({"task_id": TASK, "task_status": "succeed",
        "task_result": {list: [{"id": "v1", "url": "https://cdn.klingai.example.com/out/v1.mp4", "duration": "5"}]}}))
}
fn mp4() -> Vec<u8> {
    let mut bytes = b"\0\0\0\x20ftypisom\0\0\x02\0isomiso2avc1mp41".to_vec();
    bytes.resize(64, 0);
    bytes
}
fn png() -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[120; 16], &nodecraft_media::Limits::default()).unwrap()
}
fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
fn job(model: &str, inputs: Vec<JobInput>, params: Value) -> ProviderJob {
    let params = params.as_object().cloned().unwrap_or_default();
    ProviderJob { model: model.into(), params, inputs, idempotency_key: "nodecraft:fixture:1".into() }
}
fn text(role: &str, text: &str) -> JobInput {
    JobInput::Text { role: role.into(), text: text.into() }
}
fn image(role: &str) -> JobInput {
    JobInput::Media { role: role.into(), mime: "image/png".into(), bytes: Arc::from(png()) }
}
fn provider(t: &Arc<Transport>) -> Kling {
    Kling::new(Secret::new(KEY).unwrap(), t.clone())
}
fn signed(t: &Arc<Transport>) -> Kling {
    Kling::with_clock(Secret::new(&format!("{ACCESS}:{SECRET}")).unwrap(), t.clone(), || Some(NOW))
}
fn body(r: &HttpRequest) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}
fn authorization(r: &HttpRequest) -> &str {
    assert!(r.secret_headers.iter().any(|n| n == "authorization"), "the key travels only as a secret header");
    r.headers.iter().find(|(n, _)| n == "Authorization").map(|(_, v)| v.as_str()).unwrap()
}
fn submit(p: &Kling, j: &ProviderJob) -> JobHandle {
    let Submission::Queued(handle) = block_on(p.submit(j)).unwrap() else { panic!("expected a task to poll") };
    handle
}
fn failure(p: &Kling, j: &ProviderJob) -> ProviderError {
    block_on(p.submit(j)).unwrap_err()
}

#[test]
fn text_to_video_creates_a_task_polls_it_and_downloads_the_video_without_the_key() {
    let t = Transport::new(vec![receipt(), state("submitted"), state("processing"), finished("videos"), bytes(mp4())]);
    let p = provider(&t);
    let j = job(
        "kling-v2-5-turbo",
        vec![text("prompt", "A paper boat on a puddle"), text("negative_prompt", "blur")],
        json!({"duration": "10", "mode": "pro", "aspect_ratio": "9:16"}),
    );
    let handle = submit(&p, &j);
    assert_eq!(handle.remote_id, TASK);
    assert_eq!(block_on(p.poll(&handle)).unwrap(), JobProgress::Queued { position: None });
    assert!(matches!(block_on(p.poll(&handle)).unwrap(), JobProgress::Running { .. }));
    let JobProgress::Done(done) = block_on(p.poll(&handle)).unwrap() else { panic!("expected the video") };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: MP4.into(), bytes: Arc::from(mp4()) }]);

    let requests = t.requests();
    let (redirects, create) = &requests[0];
    assert!(!redirects);
    assert_eq!((create.method, create.url.as_str()), (HttpMethod::Post, "https://api-singapore.klingai.com/v1/videos/text2video"));
    assert_eq!(
        body(create),
        json!({"model_name": "kling-v2-5-turbo", "prompt": "A paper boat on a puddle", "negative_prompt": "blur", "duration": "10",
            "mode": "pro", "aspect_ratio": "9:16", "external_task_id": request::external_id("nodecraft:fixture:1")})
    );
    assert_eq!(requests[1].1.url, format!("https://api-singapore.klingai.com/v1/videos/text2video/{TASK}"));
    for (_, r) in &requests[..4] {
        assert_eq!(authorization(r), format!("Bearer {KEY}"));
        assert!(r.headers.iter().any(|(n, v)| n == "User-Agent" && v == crate::USER_AGENT));
    }
    let (redirects, download) = &requests[4];
    assert!(!redirects);
    assert_eq!(download.url, "https://cdn.klingai.example.com/out/v1.mp4");
    assert!(download.headers.iter().all(|(n, _)| n != "Authorization"), "the result link is fetched without the key");
    assert!(download.headers.iter().any(|(n, v)| n == "User-Agent" && v == crate::USER_AGENT));
}

#[test]
fn an_access_key_pair_signs_a_short_lived_hs256_token() {
    let t = Transport::new(vec![receipt()]);
    submit(&signed(&t), &job("kling-v2-1-master", vec![text("prompt", "Dawn")], json!({})));
    let token = authorization(&t.requests()[0].1).strip_prefix("Bearer ").unwrap().to_owned();
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3);
    let decode = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&decode(parts[0])).unwrap(), json!({"alg": "HS256", "typ": "JWT"}));
    assert_eq!(serde_json::from_slice::<Value>(&decode(parts[1])).unwrap(), json!({"iss": ACCESS, "exp": NOW + 1_800, "nbf": NOW - 5}));
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    mac.update(format!("{}.{}", parts[0], parts[1]).as_bytes());
    mac.verify_slice(&decode(parts[2])).expect("signed with the secret key");
    assert!(!token.contains(SECRET), "the secret never travels");
}

#[test]
fn start_and_end_frames_go_inline_and_the_shape_comes_from_the_frame() {
    let t = Transport::new(vec![receipt(), finished("videos"), bytes(mp4())]);
    let p = provider(&t);
    let j = job("kling-v1-6", vec![image("first_frame"), image("last_frame")], json!({"mode": "pro", "aspect_ratio": "1:1", "cfg_scale": 0.25}));
    let handle = submit(&p, &j);
    assert!(matches!(block_on(p.poll(&handle)).unwrap(), JobProgress::Done(_)));
    let requests = t.requests();
    assert_eq!(requests[0].1.url, "https://api-singapore.klingai.com/v1/videos/image2video");
    let sent = body(&requests[0].1);
    assert_eq!((sent["image"].as_str(), sent["image_tail"].as_str()), (Some(b64(&png()).as_str()), Some(b64(&png()).as_str())));
    assert_eq!((sent.get("aspect_ratio"), sent.get("prompt"), &sent["cfg_scale"]), (None, None, &json!(0.25)));
    assert_eq!(requests[1].1.url, format!("https://api-singapore.klingai.com/v1/videos/image2video/{TASK}"));
}

#[test]
fn references_make_a_multi_image_video() {
    let t = Transport::new(vec![receipt()]);
    let refs = vec![text("prompt", "They meet"), image("reference"), image("reference"), image("reference")];
    let handle = submit(&provider(&t), &job("kling-v1-6", refs, json!({"aspect_ratio": "16:9"})));
    assert_eq!(handle.data, json!({"v": 1, "path": "videos/multi-image2video"}));
    let sent = body(&t.requests()[0].1);
    assert_eq!(sent["image_list"], json!([{"image": b64(&png())}, {"image": b64(&png())}, {"image": b64(&png())}]));
    assert_eq!(sent["aspect_ratio"], "16:9");
}

#[test]
fn text_to_image_reads_the_image_list() {
    let t = Transport::new(vec![receipt(), finished("images"), bytes(png())]);
    let p = provider(&t);
    let handle = submit(&p, &job("kling-v2-1", vec![text("prompt", "A lighthouse")], json!({"aspect_ratio": "3:2"})));
    let JobProgress::Done(done) = block_on(p.poll(&handle)).unwrap() else { panic!("expected the image") };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: "image/png".into(), bytes: Arc::from(png()) }]);
    let requests = t.requests();
    assert_eq!(requests[0].1.url, "https://api-singapore.klingai.com/v1/images/generations");
    assert_eq!(body(&requests[0].1), json!({"model_name": "kling-v2-1", "prompt": "A lighthouse", "n": 1, "aspect_ratio": "3:2"}));
    assert_eq!(requests[1].1.url, format!("https://api-singapore.klingai.com/v1/images/generations/{TASK}"));
}

#[test]
fn jobs_kling_would_refuse_never_leave_the_machine() {
    let t = Transport::new(vec![]);
    let p = provider(&t);
    let cases = [
        job("kling-v9", vec![text("prompt", "x")], json!({})),
        job("kling-v2-5-turbo", vec![], json!({})),
        job("kling-v2-5-turbo", vec![text("prompt", "  ")], json!({})),
        job("kling-v2-5-turbo", vec![text("prompt", &"x".repeat(2_501))], json!({})),
        job("kling-v2-5-turbo", vec![image("first_frame"), image("last_frame")], json!({"mode": "std"})),
        job("kling-v2-5-turbo", vec![image("last_frame")], json!({"mode": "pro"})),
        job("kling-v2-5-turbo", vec![text("prompt", "x")], json!({"cfg_scale": 0.5})),
        job("kling-v2-5-turbo", vec![text("prompt", "x")], json!({"duration": "7"})),
        job("kling-v2-1-master", vec![text("prompt", "x")], json!({"mode": "pro"})),
        job("kling-v2-1-master", vec![image("reference")], json!({})),
        job("kling-v1-6", vec![image("first_frame"), image("reference")], json!({})),
        job("kling-v1-6", vec![image("reference")], json!({})),
        job("kling-v1-6", vec![text("prompt", "x")], json!({"cfg_scale": 1.5})),
        job(
            "kling-v1-6",
            vec![text("prompt", "x"), image("reference"), image("reference"), image("reference"), image("reference"), image("reference")],
            json!({}),
        ),
        job("kling-v2-1", vec![text("prompt", "x"), image("first_frame")], json!({})),
        job("kling-v2-1", vec![text("prompt", "x")], json!({"duration": "5"})),
        job("kling-v2-1", vec![text("prompt", "x")], json!({"aspect_ratio": "5:4"})),
        job("kling-v2-5-turbo", vec![JobInput::Media { role: "first_frame".into(), mime: "image/jpeg".into(), bytes: Arc::from(png()) }], json!({})),
        job("kling-v2-5-turbo", vec![JobInput::Media { role: "first_frame".into(), mime: "image/gif".into(), bytes: Arc::from(png()) }], json!({})),
    ];
    for j in &cases {
        assert_eq!(failure(&p, j).kind, ErrorKind::UnsupportedInput, "{:?} {:?}", j.model, j.params);
    }
    let small = Kling::with_limits(Secret::new(KEY).unwrap(), t.clone(), Limits { image_bytes: 16, ..Limits::HARD }).unwrap();
    assert_eq!(failure(&small, &job("kling-v2-5-turbo", vec![image("first_frame")], json!({}))).kind, ErrorKind::UnsupportedInput);
    assert!(Kling::with_limits(Secret::new(KEY).unwrap(), t.clone(), Limits { output_bytes: usize::MAX, ..Limits::HARD }).is_err());
    assert!(t.requests().is_empty());
}

#[test]
fn kling_codes_become_the_errors_a_person_can_act_on() {
    let cases = [
        (401, 1004, ErrorKind::Auth),
        (429, 1102, ErrorKind::Budget),
        (400, 1201, ErrorKind::UnsupportedInput),
        (400, 1301, ErrorKind::BlockedContent),
        (429, 1302, ErrorKind::RateLimited),
        (429, 1303, ErrorKind::Busy),
        (500, 5000, ErrorKind::Server),
        (503, 5001, ErrorKind::Busy),
        (504, 5002, ErrorKind::Timeout),
        (200, 1301, ErrorKind::BlockedContent),
        (404, 9999, ErrorKind::NotFound),
    ];
    for (status, code, kind) in cases {
        let t = Transport::new(vec![refused(status, code, "nope")]);
        let error = failure(&provider(&t), &job("kling-v2-5-turbo", vec![text("prompt", "x")], json!({})));
        assert_eq!(error.kind, kind, "HTTP {status} code {code}");
        assert!(error.message.contains(&format!("code {code}")) && error.message.contains("nope"), "{}", error.message);
        assert_eq!(error.retry_after_ms, Some(7_000));
    }
    let t = Transport::new(vec![refused(500, 5000, &format!("bad token {SECRET}"))]);
    let error = failure(&signed(&t), &job("kling-v2-5-turbo", vec![text("prompt", "x")], json!({})));
    assert!(error.message.contains("withheld") && !error.message.contains(SECRET), "{}", error.message);
    assert!(error.message.contains("may have been created"), "a server error after a create may hide a billed task");
}

#[test]
fn a_lost_create_reply_says_the_task_may_be_billed_and_is_not_retried() {
    let t = Transport::new(vec![Err(ProviderError::new(ErrorKind::Network, "reset"))]);
    let p = provider(&t);
    let error = failure(&p, &job("kling-v2-5-turbo", vec![text("prompt", "x")], json!({})));
    assert_eq!(error.kind, ErrorKind::Network);
    assert!(error.message.contains("billed"));
    assert!(!p.retry_ambiguous_submit());
}

#[test]
fn a_failed_task_reports_kling_s_reason() {
    let t = Transport::new(vec![envelope(json!({"task_id": TASK, "task_status": "failed", "task_status_msg": "Failure to pass the risk control system"}))]);
    let p = provider(&t);
    let handle = JobHandle { remote_id: TASK.into(), data: json!({"v": 1, "path": "videos/text2video"}) };
    let JobProgress::Failed(error) = block_on(p.poll(&handle)).unwrap() else { panic!("expected a failure") };
    assert_eq!(error.kind, ErrorKind::JobFailed);
    assert!(error.message.contains("risk control"));
}

#[test]
fn replies_out_of_shape_are_malformed() {
    let handle = JobHandle { remote_id: TASK.into(), data: json!({"v": 1, "path": "videos/text2video"}) };
    let replies = [
        ok(json!({"message": "no code"})),
        envelope(json!({"task_id": "other", "task_status": "processing"})),
        envelope(json!({"task_id": TASK, "task_status": "paused"})),
        envelope(json!({"task_id": TASK, "task_status": "succeed", "task_result": {"videos": []}})),
        envelope(json!({"task_id": TASK, "task_status": "succeed", "task_result": {"videos": [{"url": "http://127.0.0.1/v.mp4"}]}})),
        envelope(json!({"task_id": TASK, "task_status": "succeed", "task_result": {"videos": [{"url": format!("https://cdn.example.com/{KEY}.mp4")}]}})),
        Ok(HttpResponse { status: 200, headers: vec![], body: vec![b' '; 2 * MIB] }),
    ];
    for reply in replies {
        let t = Transport::new(vec![reply]);
        assert_eq!(block_on(provider(&t).poll(&handle)).unwrap_err().kind, ErrorKind::Malformed);
    }
    let t = Transport::new(vec![finished("videos"), bytes(png())]);
    assert_eq!(block_on(provider(&t).poll(&handle)).unwrap_err().kind, ErrorKind::Malformed, "a video that isn't an MP4");
    let t = Transport::new(vec![finished("videos"), Ok(HttpResponse { status: 403, headers: vec![], body: vec![] })]);
    assert_eq!(block_on(provider(&t).poll(&handle)).unwrap_err().kind, ErrorKind::Server, "a failed download is retried on the next poll");
    let t = Transport::new(vec![envelope(json!({"task_id": "../../v1/account"}))]);
    assert_eq!(failure(&provider(&t), &job("kling-v2-5-turbo", vec![text("prompt", "x")], json!({}))).kind, ErrorKind::Malformed);
}

#[test]
fn forged_handles_and_keys_never_reach_a_url() {
    let t = Transport::new(vec![]);
    let p = provider(&t);
    for (id, data) in [
        ("../account", json!({"v": 1, "path": "videos/text2video"})),
        (TASK, json!({"v": 1, "path": "account/costs"})),
        (TASK, json!({"v": 2, "path": "videos/text2video"})),
        (TASK, json!({})),
    ] {
        let handle = JobHandle { remote_id: id.into(), data };
        assert!(block_on(p.poll(&handle)).is_err());
    }
    let bad = Kling::new(Secret::new("a:b:c").unwrap(), t.clone());
    assert_eq!(failure(&bad, &job("kling-v2-5-turbo", vec![text("prompt", "x")], json!({}))).kind, ErrorKind::Auth);
    let unset = Kling::with_clock(Secret::new("a:b").unwrap(), t.clone(), || None);
    assert_eq!(failure(&unset, &job("kling-v2-5-turbo", vec![text("prompt", "x")], json!({}))).kind, ErrorKind::Auth);
    let handle = JobHandle { remote_id: TASK.into(), data: json!({"v": 1, "path": "videos/text2video"}) };
    assert_eq!(block_on(p.cancel(&handle)).unwrap_err().kind, ErrorKind::UnsupportedInput);
    assert!(t.requests().is_empty());
}

#[test]
fn the_same_job_always_carries_the_same_external_task_id() {
    let id = request::external_id("nodecraft:run:1");
    assert_eq!(id, request::external_id("nodecraft:run:1"));
    assert_ne!(id, request::external_id("nodecraft:run:2"));
    assert!(id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()));
}

#[test]
fn only_a_documented_refusal_of_the_create_proves_nothing_was_billed() {
    let x = || job("kling-v2-5-turbo", vec![text("prompt", "x")], json!({}));
    let not_accepted = |reply: Result<HttpResponse, ProviderError>| {
        let t = Transport::new(vec![reply]);
        let scope = SubmitContext::new();
        assert!(block_on(provider(&t).submit_with_delivery(&x(), &scope)).is_err());
        scope.finish_error().is_some()
    };
    assert!(not_accepted(refused(401, 1004, "token expired")));
    assert!(not_accepted(refused(429, 1102, "balance not enough")));
    assert!(not_accepted(refused(400, 1201, "duration is invalid")));
    assert!(!not_accepted(refused(400, 1201, "external_task_id already exists")), "the first create may have been billed");
    assert!(!not_accepted(refused(500, 5000, "internal error")));
    assert!(!not_accepted(Ok(HttpResponse { status: 400, headers: vec![], body: b"<html>".to_vec() })));
    assert!(!not_accepted(Err(ProviderError::new(ErrorKind::Network, "reset"))));
    // A clock that can't sign fails before the create is marked as sent.
    let t = Transport::new(vec![]);
    let scope = SubmitContext::new();
    let unset = Kling::with_clock(Secret::new(&format!("{ACCESS}:{SECRET}")).unwrap(), t.clone(), || None);
    assert_eq!(block_on(unset.submit_with_delivery(&x(), &scope)).unwrap_err().kind, ErrorKind::Auth);
    assert!(scope.finish_error().is_some() && t.requests().is_empty());
}

#[test]
fn a_message_echoing_a_signed_token_is_withheld() {
    let token = auth::token(ACCESS, SECRET, NOW).unwrap();
    let t = Transport::new(vec![refused(401, 1004, &format!("bad token {token}"))]);
    let error = failure(&provider(&t), &job("kling-v2-5-turbo", vec![text("prompt", "x")], json!({})));
    assert!(error.message.contains("withheld") && !error.message.contains(&token), "{}", error.message);
}
