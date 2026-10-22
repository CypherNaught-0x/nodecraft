use super::*;
use crate::{HttpMethod, JobInput};
use base64::Engine as _;
use futures::executor::block_on;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};

const KEY: &str = "gem-fixture-key-not-real";
const FILE: &str = "a1b2c3d4e5";

fn key() -> Secret {
    Secret::new(KEY).unwrap()
}
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
fn status(code: u16) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: code, headers: vec![("Retry-After".into(), "7".into())], body: b"{\"error\":{}}".to_vec() })
}
fn mp4() -> Vec<u8> {
    let mut bytes = b"\0\0\0\x20ftypisom\0\0\x02\0isomiso2avc1mp41".to_vec();
    bytes.resize(64, 0);
    bytes
}
fn png() -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[120; 16], &nodecraft_media::Limits::default()).unwrap()
}
fn job(inputs: Vec<JobInput>) -> ProviderJob {
    ProviderJob { model: OMNI_FLASH.into(), params: Default::default(), inputs, idempotency_key: "fixture-operation".into() }
}
fn prompt(text: &str) -> JobInput {
    JobInput::Text { role: "prompt".into(), text: text.into() }
}
fn image(role: &str) -> JobInput {
    JobInput::Media { role: role.into(), mime: "image/png".into(), bytes: Arc::from(png()) }
}
fn usage() -> Value {
    json!({"total_input_tokens": 12, "total_output_tokens": 46_336, "total_tokens": 46_348})
}
fn created(content: Value) -> Result<HttpResponse, ProviderError> {
    ok(json!({"id": "v1_abc", "status": "completed", "steps": [
        {"type": "user_input", "content": [{"type": "text", "text": "a marble"}]},
        {"type": "model_output", "content": [content]}
    ], "usage": usage()}))
}
fn provider(t: &Arc<Transport>) -> GeminiVideo {
    GeminiVideo::new(key(), t.clone())
}
fn body(r: &HttpRequest) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}

#[test]
fn text_to_video_sends_one_request_then_polls_and_downloads_the_file() {
    let t = Transport::new(vec![
        created(json!({"type": "video", "mime_type": "video/mp4", "uri": format!("files/{FILE}")})),
        ok(json!({"name": format!("files/{FILE}"), "state": "PROCESSING"})),
        ok(json!({"name": format!("files/{FILE}"), "state": "ACTIVE"})),
        Ok(HttpResponse { status: 200, headers: vec![], body: mp4() }),
    ]);
    let p = provider(&t);
    let mut j = job(vec![prompt("A marble rolling on a track")]);
    j.params.insert("aspect_ratio".into(), json!("9:16"));
    j.params.insert("resolution".into(), json!("1080p"));
    let Submission::Queued(handle) = block_on(p.submit(&j)).unwrap() else { panic!("expected a file to poll") };
    assert_eq!(handle.remote_id, FILE);
    assert!(matches!(block_on(p.poll(&handle)).unwrap(), JobProgress::Running { .. }));
    let JobProgress::Done(done) = block_on(p.poll(&handle)).unwrap() else { panic!("expected the video") };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: MP4.into(), bytes: Arc::from(mp4()) }]);
    assert_eq!((done.usage.input_tokens, done.usage.output_tokens), (Some(12), Some(46_336)), "the generate request's tokens reach the result");

    let requests = t.requests();
    let (redirects, create) = &requests[0];
    assert!(!redirects);
    assert_eq!((create.method, create.url.as_str()), (HttpMethod::Post, "https://generativelanguage.googleapis.com/v1beta/interactions"));
    assert_eq!(
        body(create),
        json!({"model": OMNI_FLASH, "store": false, "background": false, "stream": false,
            "response_format": {"type": "video", "delivery": "uri", "aspect_ratio": "9:16", "resolution": "1080p"},
            "input": [{"type": "text", "text": "A marble rolling on a track"}]})
    );
    assert_eq!(requests[1].1.url, format!("https://generativelanguage.googleapis.com/v1beta/files/{FILE}"));
    let (redirects, download) = &requests[3];
    assert!(redirects, "the download may be redirected to Google's storage, without the key");
    assert_eq!(download.url, format!("https://generativelanguage.googleapis.com/v1beta/files/{FILE}:download?alt=media"));
    for (_, r) in &requests {
        assert!(r.headers.iter().any(|(n, v)| n == "x-goog-api-key" && v == KEY));
        assert!(r.secret_headers.iter().any(|n| n == "x-goog-api-key"), "the key travels only as a secret header");
        assert!(r.headers.iter().any(|(n, v)| n == "User-Agent" && v == crate::USER_AGENT));
        assert!(!r.url.contains(KEY));
    }
}

#[test]
fn frames_and_references_go_first_in_order() {
    let t = Transport::new(vec![created(json!({"type": "video", "uri": format!("{API}/files/{FILE}")}))]);
    let j = job(vec![prompt("forest to snow"), image("last_frame"), image("first_frame")]);
    let Submission::Queued(handle) = block_on(provider(&t).submit(&j)).unwrap() else { panic!() };
    assert_eq!(handle.remote_id, FILE, "a full file URL on the API's origin is read too");
    let input = body(&t.requests()[0].1)["input"].clone();
    let data = base64::engine::general_purpose::STANDARD.encode(png());
    let item = json!({"type": "image", "mime_type": "image/png", "data": data});
    assert_eq!(input, json!([item, item, {"type": "text", "text": "forest to snow"}]));

    let t = Transport::new(vec![created(json!({"type": "video", "uri": format!("files/{FILE}")}))]);
    let j = job(vec![image("reference"), prompt("a cat and yarn"), image("reference")]);
    block_on(provider(&t).submit(&j)).unwrap();
    let input = body(&t.requests()[0].1)["input"].clone();
    assert_eq!(input.as_array().unwrap().len(), 3);
    assert_eq!(input[2]["type"], "text");
}

#[test]
fn an_inline_video_finishes_at_submit() {
    let data = base64::engine::general_purpose::STANDARD.encode(mp4());
    let t = Transport::new(vec![created(json!({"type": "video", "mime_type": "video/mp4", "data": data}))]);
    let Submission::Finished(done) = block_on(provider(&t).submit(&job(vec![prompt("a marble")]))).unwrap() else { panic!() };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: MP4.into(), bytes: Arc::from(mp4()) }]);
    assert_eq!(done.usage.output_tokens, Some(46_336));
}

#[test]
fn bad_jobs_are_refused_before_anything_is_sent() {
    let refused = |j: ProviderJob| {
        let t = Transport::new(vec![]);
        let e = block_on(provider(&t).submit(&j)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::UnsupportedInput, "{}", e.message);
        assert!(t.requests().is_empty());
    };
    refused(job(vec![]));
    refused(job(vec![prompt("   ")]));
    refused(job(vec![prompt("a"), prompt("b")]));
    refused(job(vec![prompt("a"), image("last_frame")]));
    refused(job(vec![prompt("a"), image("first_frame"), image("reference")]));
    refused(job(vec![prompt("a"), image("reference"), image("reference"), image("reference"), image("reference")]));
    refused(job(vec![prompt("a"), image("mask")]));
    refused(job(vec![prompt("a"), JobInput::Media { role: "first_frame".into(), mime: "image/jpeg".into(), bytes: Arc::from(png()) }]));
    refused(job(vec![prompt("a"), JobInput::Media { role: "first_frame".into(), mime: "image/webp".into(), bytes: Arc::from(png()) }]));
    refused(ProviderJob { model: "veo-3.1".into(), ..job(vec![prompt("a")]) });
    for (name, value) in [("resolution", json!("4k")), ("aspect_ratio", json!("1:1")), ("duration", json!(8)), ("resolution", json!(720))] {
        let mut j = job(vec![prompt("a")]);
        j.params.insert(name.into(), value);
        refused(j);
    }
    let small = Limits { image_bytes: 10, ..Limits::HARD };
    let t = Transport::new(vec![]);
    let e = block_on(GeminiVideo::with_limits(key(), t.clone(), small).unwrap().submit(&job(vec![prompt("a"), image("first_frame")]))).unwrap_err();
    assert_eq!(e.kind, ErrorKind::UnsupportedInput);
    assert!(GeminiVideo::with_limits(key(), t, Limits { output_bytes: usize::MAX, ..Limits::HARD }).is_err(), "limits may only be lowered");
}

#[test]
fn failed_and_hostile_replies_are_errors_not_panics() {
    let errs = |reply: Result<HttpResponse, ProviderError>| {
        let t = Transport::new(vec![reply]);
        block_on(provider(&t).submit(&job(vec![prompt("a")]))).unwrap_err()
    };
    let e = errs(status(429));
    assert_eq!((e.kind, e.retry_after_ms), (ErrorKind::RateLimited, Some(7000)));
    assert_eq!(errs(status(403)).kind, ErrorKind::Auth);
    assert_eq!(errs(status(400)).kind, ErrorKind::UnsupportedInput);
    assert_eq!(errs(ok(json!({"status": "failed"}))).kind, ErrorKind::JobFailed);
    assert_eq!(errs(ok(json!({"status": "cancelled"}))).kind, ErrorKind::Cancelled);
    assert_eq!(errs(ok(json!({"status": "completed", "steps": []}))).kind, ErrorKind::BlockedContent, "no video");
    assert_eq!(errs(status(302)).kind, ErrorKind::Malformed, "a redirect on the paid request isn't followed");
    let huge = Limits { output_bytes: 16, ..Limits::HARD };
    let data = base64::engine::general_purpose::STANDARD.encode(mp4());
    let t = Transport::new(vec![created(json!({"type": "video", "data": data}))]);
    let e = block_on(GeminiVideo::with_limits(key(), t, huge).unwrap().submit(&job(vec![prompt("a")]))).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Malformed, "an oversize inline video is refused before decoding");
    for content in [
        json!({"type": "video", "uri": "files/../../etc"}),
        json!({"type": "video", "uri": format!("files/{KEY}")}),
        json!({"type": "video", "uri": "https://evil.example/files/abc"}),
        json!({"type": "video", "uri": "files/"}),
        json!({"type": "video", "mime_type": "video/webm", "uri": "files/abc"}),
        json!({"type": "video", "data": "not base64!"}),
        json!({"type": "video", "data": base64::engine::general_purpose::STANDARD.encode(png())}),
        json!({"type": "video"}),
        json!({"type": "video", "uri": "files/abc", "data": "AAAA"}),
    ] {
        assert_eq!(errs(created(content.clone())).kind, ErrorKind::Malformed, "{content}");
    }
    let two = ok(json!({"status": "completed", "steps": [{"type": "model_output", "content": [
        {"type": "video", "uri": "files/a"}, {"type": "video", "uri": "files/b"}]}]}));
    assert_eq!(errs(two).kind, ErrorKind::Malformed);
    for garbage in [b"".to_vec(), b"[]".to_vec(), b"{\"status\":7}".to_vec(), vec![0xFF; 64]] {
        assert_eq!(errs(Ok(HttpResponse { status: 200, headers: vec![], body: garbage })).kind, ErrorKind::Malformed);
    }
    let lost = errs(Err(ProviderError::new(ErrorKind::Network, "reset")));
    assert!(lost.message.contains("may have been generated and billed"), "{}", lost.message);
}

#[test]
fn polls_report_failure_and_refuse_forged_handles() {
    let handle = JobHandle { remote_id: FILE.into(), data: json!({"v": 1}) };
    let t = Transport::new(vec![ok(json!({"state": "FAILED"}))]);
    assert!(matches!(block_on(provider(&t).poll(&handle)).unwrap(), JobProgress::Failed(e) if e.kind == ErrorKind::JobFailed));

    let t = Transport::new(vec![ok(json!({"state": "WEIRD"}))]);
    assert_eq!(block_on(provider(&t).poll(&handle)).unwrap_err().kind, ErrorKind::Malformed);

    // A download that fails stays retryable: the file is still there.
    let t = Transport::new(vec![ok(json!({"state": "ACTIVE"})), status(500)]);
    assert_eq!(block_on(provider(&t).poll(&handle)).unwrap_err().kind, ErrorKind::Server);
    let t = Transport::new(vec![ok(json!({"state": "ACTIVE"})), Ok(HttpResponse { status: 200, headers: vec![], body: png() })]);
    assert_eq!(block_on(provider(&t).poll(&handle)).unwrap_err().kind, ErrorKind::Malformed, "not an MP4");

    for forged in [
        JobHandle { remote_id: "../secrets".into(), data: json!({"v": 1}) },
        JobHandle { remote_id: KEY.into(), data: json!({"v": 1}) },
        JobHandle { remote_id: FILE.into(), data: json!({"v": 2}) },
        JobHandle { remote_id: FILE.into(), data: json!(null) },
    ] {
        let t = Transport::new(vec![]);
        assert!(block_on(provider(&t).poll(&forged)).is_err());
        assert!(t.requests().is_empty(), "a forged handle never reaches a URL");
    }
    let t = Transport::new(vec![]);
    assert_eq!(block_on(provider(&t).cancel(&handle)).unwrap_err().kind, ErrorKind::UnsupportedInput);
}

#[test]
fn delivery_turns_unknown_before_the_paid_send() {
    let t = Transport::new(vec![Err(ProviderError::new(ErrorKind::Network, "reset"))]);
    let scope = crate::SubmitContext::new();
    assert!(block_on(provider(&t).submit_with_delivery(&job(vec![prompt("a")]), &scope)).is_err());
    assert!(scope.finish_error().is_none(), "the video may exist");
    assert!(!provider(&t).retry_ambiguous_submit());
    let t = Transport::new(vec![]);
    let scope = crate::SubmitContext::new();
    assert!(block_on(provider(&t).submit_with_delivery(&job(vec![]), &scope)).is_err());
    assert!(scope.finish_error().is_some(), "a job refused locally was never sent");
}

#[test]
fn every_file_reference_form_on_googles_origin_is_read_and_others_refused() {
    for uri in [
        format!("files/{FILE}"),
        format!("{API}/files/{FILE}"),
        format!("https://generativelanguage.googleapis.com/v1/files/{FILE}"),
        format!("https://generativelanguage.googleapis.com/download/v1beta/files/{FILE}:download?alt=media"),
        format!("{API}/files/{FILE}:download"),
    ] {
        let t = Transport::new(vec![created(json!({"type": "video", "uri": uri}))]);
        let Submission::Queued(handle) = block_on(provider(&t).submit(&job(vec![prompt("a")]))).unwrap() else { panic!("{uri}") };
        assert_eq!(handle.remote_id, FILE, "{uri}");
    }
    for uri in [
        format!("https://generativelanguage.googleapis.com.evil.example/v1beta/files/{FILE}"),
        format!("http://generativelanguage.googleapis.com/v1beta/files/{FILE}"),
        format!("{API}/notfiles/{FILE}"),
        format!("{API}/files/{FILE}/more"),
        FILE.to_string(),
    ] {
        let t = Transport::new(vec![created(json!({"type": "video", "uri": uri}))]);
        assert_eq!(block_on(provider(&t).submit(&job(vec![prompt("a")]))).unwrap_err().kind, ErrorKind::Malformed, "{uri}");
    }
}

#[test]
fn the_top_level_output_video_is_read_and_odd_usage_never_loses_the_video() {
    let t = Transport::new(vec![ok(json!({"status": "completed", "output_video": {"type": "video", "uri": format!("files/{FILE}")},
        "usage": {"total_input_tokens": "12", "total_output_tokens": -1}}))]);
    let Submission::Queued(handle) = block_on(provider(&t).submit(&job(vec![prompt("a")]))).unwrap() else { panic!() };
    assert_eq!(handle.remote_id, FILE);
    // A handle without token counts still finishes, reporting none.
    let t = Transport::new(vec![
        ok(json!({"state": "STATE_UNSPECIFIED"})),
        ok(json!({})),
        ok(json!({"state": "ACTIVE"})),
        Ok(HttpResponse { status: 200, headers: vec![], body: mp4() }),
    ]);
    let p = provider(&t);
    assert!(matches!(block_on(p.poll(&handle)).unwrap(), JobProgress::Running { .. }));
    assert!(matches!(block_on(p.poll(&handle)).unwrap(), JobProgress::Running { .. }));
    let JobProgress::Done(done) = block_on(p.poll(&handle)).unwrap() else { panic!() };
    assert_eq!(done.usage, Usage::default());
}

#[test]
fn failed_downloads_say_why() {
    let handle = JobHandle { remote_id: FILE.into(), data: json!({"v": 1}) };
    for (code, kind) in [(403, ErrorKind::Auth), (404, ErrorKind::NotFound), (500, ErrorKind::Server)] {
        let t = Transport::new(vec![ok(json!({"state": "ACTIVE"})), status(code)]);
        assert_eq!(block_on(provider(&t).poll(&handle)).unwrap_err().kind, kind, "{code}");
    }
    let t = Transport::new(vec![ok(json!({"state": "ACTIVE"})), Err(ProviderError::new(ErrorKind::Malformed, "the reply is larger than 268435456 bytes"))]);
    let e = block_on(provider(&t).poll(&handle)).unwrap_err();
    assert!(e.message.contains("size limit"), "{}", e.message);
    let t = Transport::new(vec![status(404)]);
    let e = block_on(provider(&t).poll(&handle)).unwrap_err();
    assert!(e.message.contains("two days"), "{}", e.message);
}
