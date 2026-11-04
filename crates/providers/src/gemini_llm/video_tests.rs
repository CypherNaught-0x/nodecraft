//! Video to text through the Interactions API (#84), against public synthetic fixtures. No
//! live calls.
use super::*;
use crate::{HttpResponse, Output};
use futures::executor::block_on;
use serde_json::json;
use std::sync::Mutex;

const REPLY: &str = include_str!("fixtures/video_reply.json");
/// An MP4 file's first bytes: enough to stand for a video; the adapter never decodes it.
const MP4: &[u8] = b"\0\0\0\x20ftypisom\0\0\x02\0isomiso2avc1mp41";

/// Replies with one canned response to a no-redirect send, and records what it was sent. A
/// plain `send` is a test failure: the paid request never follows redirects.
struct Fixture {
    reply: Mutex<Option<Result<HttpResponse, ProviderError>>>,
    seen: Mutex<Vec<HttpRequest>>,
}
impl Fixture {
    fn new(status: u16, body: &str) -> Arc<Self> {
        Arc::new(Self { reply: Mutex::new(Some(Ok(HttpResponse { status, headers: vec![], body: body.as_bytes().to_vec() }))), seen: Mutex::default() })
    }
    fn failing(kind: ErrorKind) -> Arc<Self> {
        Arc::new(Self { reply: Mutex::new(Some(Err(ProviderError::new(kind, "public transport failure")))), seen: Mutex::default() })
    }
    fn sent(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
    fn body(&self) -> serde_json::Value {
        serde_json::from_slice(&self.seen.lock().unwrap()[0].body).unwrap()
    }
}
impl Http for Fixture {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("the video request must not follow redirects")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.seen.lock().unwrap().push(request);
        let reply = self.reply.lock().unwrap().take().unwrap();
        Box::pin(async move { reply })
    }
}

fn provider(http: Arc<dyn Http>) -> GeminiLlm {
    GeminiLlm::new(&Secret::new("public-fixture-key").unwrap(), http)
}
fn video(mime: &str, bytes: &[u8]) -> JobInput {
    JobInput::Media { role: video::ROLE.into(), mime: mime.into(), bytes: Arc::from(bytes) }
}
fn job(videos: Vec<JobInput>) -> ProviderJob {
    let mut inputs = vec![JobInput::Text { role: "prompt".into(), text: "Describe the clip".into() }];
    inputs.extend(videos);
    ProviderJob { model: MODEL.into(), params: Default::default(), inputs, idempotency_key: "public-fixture".into() }
}

#[test]
fn gemini84_video_goes_inline_to_interactions_and_the_answer_is_its_text() {
    let http = Fixture::new(200, REPLY);
    let p = provider(http.clone());
    let mut j = job(vec![video("video/mp4", MP4), video("video/quicktime", b"moov")]);
    j.params.insert("max_tokens".into(), json!(900));
    let Submission::Finished(done) = block_on(p.submit(&j)).unwrap() else { panic!("synchronous") };
    assert_eq!(done.outputs, [Output::Text("A small boat crosses a calm harbour at dawn.".into())]);
    assert_eq!((done.usage.input_tokens, done.usage.output_tokens, done.usage.cost), (Some(612), Some(11), None));
    let request = http.seen.lock().unwrap()[0].clone();
    assert_eq!(request.url, "https://generativelanguage.googleapis.com/v1beta/interactions");
    assert_eq!(request.secret_headers, ["x-goog-api-key"]);
    assert!(!format!("{request:?}").contains("public-fixture-key"), "the key never shows in diagnostics");
    assert_eq!(request.max_response_bytes, video::MAX_RESPONSE_BYTES);
    let body = http.body();
    assert_eq!(
        body,
        json!({
            "model": MODEL,
            "store": false,
            "background": false,
            "stream": false,
            "generation_config": {"max_output_tokens": 900, "thinking_summaries": "none"},
            "input": [
                {"type": "text", "text": "Describe the clip"},
                {"type": "video", "mime_type": "video/mp4", "data": base64::engine::general_purpose::STANDARD.encode(MP4)},
                {"type": "video", "mime_type": "video/mov", "data": "bW9vdg=="},
            ],
        })
    );
    // The answer limit defaults as on the text route, and a streaming delivery still gets the
    // whole answer once the request returns.
    let http = Fixture::new(200, REPLY);
    let context = SubmitContext::streaming_text();
    let Submission::Finished(done) = block_on(provider(http.clone()).submit_with_delivery(&job(vec![video("video/webm", b"webm")]), &context)).unwrap() else {
        panic!("synchronous")
    };
    assert_eq!(done.outputs.len(), 1);
    assert_eq!(http.body()["generation_config"]["max_output_tokens"], DEFAULT_TOKENS);
}

use base64::Engine as _;

#[test]
fn gemini84_unsupported_or_oversized_videos_are_refused_before_anything_is_sent() {
    let http = Fixture::new(200, REPLY);
    let p = provider(http.clone());
    let mut cases = vec![
        job(vec![video("video/x-matroska", b"mkv")]),
        job(vec![video("video/ogg", b"ogg")]),
        job(vec![video("video/mp4", b"")]),
        job(vec![video("video/mp4", MP4); video::MAX_VIDEOS + 1]),
        // Base64 makes 15 MB of video 20 MB of request: over the inline limit.
        job(vec![video("video/mp4", &vec![0; 15_000_000])]),
        job(vec![video("video/mp4", &vec![0; 8_000_000]), video("video/mp4", &vec![0; 8_000_000])]),
        job(vec![JobInput::Media { role: "image".into(), mime: "video/mp4".into(), bytes: Arc::from(MP4) }]),
    ];
    let mut j = job(vec![]);
    j.inputs.push(JobInput::Media { role: video::ROLE.into(), mime: "image/png".into(), bytes: Arc::from(&b"PNG"[..]) });
    cases.push(j);
    let mut j = job(vec![video("video/mp4", MP4)]);
    j.inputs.remove(0);
    cases.push(j);
    let mut j = job(vec![video("video/mp4", MP4)]);
    j.model = "gemini-other".into();
    cases.push(j);
    for cap in [json!(0), json!(65_537), json!("4096")] {
        let mut j = job(vec![video("video/mp4", MP4)]);
        j.params.insert("max_tokens".into(), cap);
        cases.push(j);
    }
    let mut j = job(vec![video("video/mp4", MP4)]);
    j.params.insert("temperature".into(), json!(0.5));
    cases.push(j);
    let mut j = job(vec![video("video/mp4", MP4)]);
    j.idempotency_key = "k".repeat(257);
    cases.push(j);
    for j in cases {
        let refused = block_on(p.submit(&j)).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::UnsupportedInput, "{refused:?}");
    }
    let mkv = block_on(p.submit(&job(vec![video("video/x-matroska", b"mkv")]))).unwrap_err();
    assert!(mkv.message.contains("video/x-matroska") && mkv.message.contains("MP4"), "names the type and what works: {mkv:?}");
    let big = block_on(p.submit(&job(vec![video("video/mp4", &vec![0; 15_000_000])]))).unwrap_err();
    assert!(big.message.contains("20 MB"), "{big:?}");
    assert_eq!(http.sent(), 0, "nothing reached the network");
    // Just under the limit is sent, and the body is exactly as long as it was measured.
    let fits = video::body(&job(vec![video("video/mp4", &vec![7; 14_900_000])]), 10).unwrap();
    assert!(fits.len() <= video::MAX_REQUEST_BYTES);
    serde_json::from_slice::<serde_json::Value>(&fits).unwrap();
}

#[test]
fn gemini84_video_replies_are_read_honestly() {
    let status = |s: &str| format!(r#"{{"status":"{s}","steps":[{{"type":"model_output","content":[{{"type":"text","text":"x"}}]}}]}}"#);
    for (body, kind) in [
        (status("failed"), ErrorKind::JobFailed),
        (status("cancelled"), ErrorKind::Cancelled),
        (status("incomplete"), ErrorKind::Malformed),
        (status("in_progress"), ErrorKind::Malformed),
        ("not JSON".into(), ErrorKind::Malformed),
        (r#"{"status":"completed","steps":[]}"#.into(), ErrorKind::Malformed),
        (r#"{"status":"completed","steps":[{"type":"model_output","content":[{"type":"image","data":"AAAA"}]}]}"#.into(), ErrorKind::Malformed),
        (r#"{"status":"completed","steps":[{"type":"function_call"}]}"#.into(), ErrorKind::Malformed),
        (r#"{"status":"completed","steps":[{"type":"model_output"}]}"#.into(), ErrorKind::Malformed),
    ] {
        let refused = block_on(provider(Fixture::new(200, &body)).submit(&job(vec![video("video/mp4", MP4)]))).unwrap_err();
        assert_eq!(refused.kind, kind, "{body}");
    }
    // Missing or malformed usage never loses a good answer.
    let body = r#"{"status":"completed","steps":[{"type":"model_output","content":[{"type":"text","text":"ok"}]}],"usage":{"total_input_tokens":"many"}}"#;
    let Submission::Finished(done) = block_on(provider(Fixture::new(200, body)).submit(&job(vec![video("video/mp4", MP4)]))).unwrap() else { panic!() };
    assert_eq!((done.outputs, done.usage.input_tokens), (vec![Output::Text("ok".into())], None));
    // Google's documented error code is shown; its message never is.
    let body = r#"{"error":{"code":"invalid_request","message":"public fixture: Describe the clip"}}"#;
    let refused = block_on(provider(Fixture::new(400, body)).submit(&job(vec![video("video/mp4", MP4)]))).unwrap_err();
    assert!(refused.message.contains("HTTP 400, invalid_request") && !refused.message.contains("Describe"), "{refused:?}");
    let odd = r#"{"error":{"code":"Not A Code; see https://x","message":"m"}}"#;
    assert!(block_on(provider(Fixture::new(400, odd)).submit(&job(vec![video("video/mp4", MP4)]))).unwrap_err().message.contains("(HTTP 400)"));
    // Thought tokens count as output only when the total shows them apart.
    let reply =
        |usage: &str| format!(r#"{{"status":"completed","steps":[{{"type":"model_output","content":[{{"type":"text","text":"ok"}}]}}],"usage":{usage}}}"#);
    for (usage, output) in [
        (r#"{"total_input_tokens":10,"total_output_tokens":5,"total_thought_tokens":7,"total_tokens":22}"#, Some(12)),
        (r#"{"total_input_tokens":10,"total_output_tokens":12,"total_thought_tokens":7,"total_tokens":22}"#, Some(12)),
        (r#"{"total_input_tokens":10,"total_output_tokens":5,"total_thought_tokens":7}"#, Some(5)),
    ] {
        let Submission::Finished(done) = block_on(provider(Fixture::new(200, &reply(usage))).submit(&job(vec![video("video/mp4", MP4)]))).unwrap() else {
            panic!()
        };
        assert_eq!(done.usage.output_tokens, output, "{usage}");
    }
    for (status, kind) in [
        (400, ErrorKind::UnsupportedInput),
        (401, ErrorKind::Auth),
        (404, ErrorKind::NotFound),
        (429, ErrorKind::RateLimited),
        (500, ErrorKind::Server),
        (503, ErrorKind::Busy),
    ] {
        let refused =
            block_on(provider(Fixture::new(status, r#"{"error":{"message":"public fixture"}}"#)).submit(&job(vec![video("video/mp4", MP4)]))).unwrap_err();
        assert_eq!(refused.kind, kind, "{status}");
        assert!(refused.message.contains("may have been billed"), "{refused:?}");
    }
    let p = provider(Fixture::failing(ErrorKind::Timeout));
    let lost = block_on(p.submit(&job(vec![video("video/mp4", MP4)]))).unwrap_err();
    assert_eq!(lost.kind, ErrorKind::Timeout);
    assert!(lost.message.contains("may have been billed") && !p.retry_ambiguous_submit(), "a lost reply is never replayed: {lost:?}");
}

#[test]
fn gemini84_catalog_declares_video_to_text_as_the_guide_documents() {
    let model = nodecraft_catalog::Catalog::builtin().unwrap().get("gemini-llm.3.8-flash").unwrap().clone();
    let mode = model.modes.iter().find(|m| m.id == "video-to-text").expect("a video-to-text mode");
    let role = mode.inputs.iter().find(|r| r.role == video::ROLE).unwrap();
    assert_eq!((role.modality, usize::from(role.max)), (nodecraft_types::Modality::Video, video::MAX_VIDEOS));
    let reads = mode.reads.as_ref().unwrap();
    assert_eq!(reads.frames, Some(nodecraft_types::FrameSampling { every_ms: 1000, max: 10_800 }));
    assert_eq!((reads.context_tokens, reads.max_output_tokens), (Some(1_048_576), Some(65_536)));
    assert!(!reads.timestamps && !reads.speakers, "neither is promised");
    // The text route's mode is unchanged and also declares the window.
    let text = model.modes.iter().find(|m| m.id == "text-to-text").unwrap();
    assert_eq!(text.reads.as_ref().and_then(|r| r.context_tokens), Some(1_048_576));
    assert!(video::wire_mime("video/quicktime") == Some("video/mov") && video::wire_mime("video/x-matroska").is_none());
}
