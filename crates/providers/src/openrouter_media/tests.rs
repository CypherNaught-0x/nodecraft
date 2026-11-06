use super::*;
use crate::secrets::Secret;
use crate::{BoxFuture, Http, HttpRequest, HttpResponse, JobHandle, JobProgress, Output, Provider, Submission};
use futures::executor::block_on;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const KEY: &str = "public-openrouter-fixture";
/// An ID3v2.4 header and one MPEG-1 Layer III frame header.
const MP3: &[u8] = b"ID3\x04\0\0\0\0\0\0\xFF\xFB\x90\x64\0\0\0\0";

/// Answers in order and remembers each request and whether it could follow redirects.
struct Wire {
    sent: Mutex<Vec<(HttpRequest, bool)>>,
    replies: Mutex<VecDeque<HttpResponse>>,
}

impl Wire {
    fn new(replies: impl IntoIterator<Item = HttpResponse>) -> Arc<Self> {
        Arc::new(Self { sent: Mutex::default(), replies: Mutex::new(replies.into_iter().collect()) })
    }
    fn reply(&self, request: HttpRequest, redirects: bool) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.sent.lock().unwrap().push((request, redirects));
        let reply = self.replies.lock().unwrap().pop_front().expect("unexpected request");
        Box::pin(async move { Ok(reply) })
    }
    fn sent(&self) -> Vec<(HttpRequest, bool)> {
        self.sent.lock().unwrap().clone()
    }
}

impl Http for Wire {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.reply(request, true)
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.reply(request, false)
    }
}

fn bytes(status: u16, body: &[u8]) -> HttpResponse {
    HttpResponse { status, headers: vec![], body: body.to_vec() }
}

fn reply(status: u16, value: Value) -> HttpResponse {
    bytes(status, &serde_json::to_vec(&value).unwrap())
}

fn job(model: &str, params: Value, inputs: Vec<JobInput>) -> ProviderJob {
    ProviderJob { model: model.into(), params: params.as_object().unwrap().clone(), inputs, idempotency_key: "public-key".into() }
}

fn prompt(text: &str) -> JobInput {
    JobInput::Text { role: "prompt".into(), text: text.into() }
}

fn body(request: &HttpRequest) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

fn mp4() -> Vec<u8> {
    let atom = |name: &[u8; 4], data: &[u8]| [u32::try_from(data.len() + 8).unwrap().to_be_bytes().as_slice(), name, data].concat();
    let mut mvhd = vec![0; 100];
    mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
    mvhd[16..20].copy_from_slice(&5000u32.to_be_bytes());
    let mut tkhd = vec![0; 84];
    tkhd[76..80].copy_from_slice(&(1280u32 << 16).to_be_bytes());
    tkhd[80..84].copy_from_slice(&(720u32 << 16).to_be_bytes());
    let mut hdlr = vec![0; 24];
    hdlr[8..12].copy_from_slice(b"vide");
    let trak = atom(b"trak", &[atom(b"tkhd", &tkhd), atom(b"mdia", &atom(b"hdlr", &hdlr))].concat());
    [atom(b"ftyp", b"isom\0\0\x02\0isom"), atom(b"moov", &[atom(b"mvhd", &mvhd), trak].concat()), atom(b"mdat", b"fixture")].concat()
}

fn png() -> Vec<u8> {
    nodecraft_media::encode_png(8, 8, &vec![90; 8 * 8 * 4], &nodecraft_media::Limits::default()).unwrap()
}

#[test]
fn speech_sends_the_text_and_voice_and_returns_the_mp3() {
    let wire = Wire::new([bytes(200, MP3)]);
    let speech = OpenRouterSpeech::new(Secret::new(KEY).unwrap(), wire.clone());
    let Submission::Finished(done) = block_on(speech.submit(&job("hexgrad/kokoro-82m", json!({"voice": "af_bella"}), vec![prompt("Hello there")]))).unwrap()
    else {
        panic!("synchronous")
    };
    assert!(matches!(&done.outputs[..], [Output::Bytes { mime, .. }] if mime.starts_with("audio/")), "{:?}", done.outputs);
    let sent = wire.sent();
    assert_eq!(sent[0].0.url, "https://openrouter.ai/api/v1/audio/speech");
    assert!(!sent[0].1, "never redirected with the key");
    assert_eq!(body(&sent[0].0), json!({"model": "hexgrad/kokoro-82m", "input": "Hello there", "voice": "af_bella", "response_format": "mp3"}));
    assert!(!speech.retry_ambiguous_submit());
}

#[test]
fn speech_refuses_bad_jobs_before_sending_and_reports_bad_replies() {
    let wire = Wire::new([bytes(200, b"<html>not audio</html>"), reply(400, json!({"error": {"message": "voice required"}}))]);
    let speech = OpenRouterSpeech::new(Secret::new(KEY).unwrap(), wire.clone());
    for bad in [
        job("m/x", json!({"speed": 2}), vec![prompt("x")]),
        job("m/x", json!({"voice": "a\" ,\"x\":\"y"}), vec![prompt("x")]),
        job("m/x", json!({}), vec![]),
        job("m/x", json!({}), vec![prompt(&"λ".repeat(speech::MAX_CHARS + 1))]),
        job("bad model", json!({}), vec![prompt("x")]),
        job("m/x", json!({}), vec![prompt("x"), JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(png()) }]),
    ] {
        assert_eq!(block_on(speech.submit(&bad)).expect_err("refused").kind, ErrorKind::UnsupportedInput);
    }
    assert!(wire.sent().is_empty());
    assert_eq!(block_on(speech.submit(&job("m/x", json!({}), vec![prompt("x")]))).expect_err("not audio").kind, ErrorKind::Malformed);
    assert_eq!(block_on(speech.submit(&job("m/x", json!({}), vec![prompt("x")]))).expect_err("400").kind, ErrorKind::UnsupportedInput);
}

#[test]
fn video_queues_polls_and_downloads_with_the_reported_cost() {
    let video = mp4();
    let wire = Wire::new([
        reply(202, json!({"id": "job_123", "status": "pending"})),
        reply(200, json!({"id": "job_123", "status": "pending"})),
        reply(200, json!({"id": "job_123", "status": "in_progress"})),
        reply(200, json!({"id": "job_123", "status": "completed", "usage": {"cost": 0.4}})),
        bytes(200, &video),
    ]);
    let adapter = OpenRouterVideo::new(Secret::new(KEY).unwrap(), wire.clone());
    let frame = JobInput::Media { role: "first_frame".into(), mime: "image/png".into(), bytes: Arc::from(png()) };
    let job = job(
        "alibaba/wan-2.7",
        json!({"duration": 5, "resolution": "720p", "aspect_ratio": "16:9", "generate_audio": false, "seed": 3}),
        vec![prompt("a fox"), frame],
    );
    let Submission::Queued(handle) = block_on(adapter.submit(&job)).unwrap() else { panic!("queued") };
    assert_eq!(handle.remote_id, "job_123");
    assert!(matches!(block_on(adapter.poll(&handle)).unwrap(), JobProgress::Queued { .. }));
    assert!(matches!(block_on(adapter.poll(&handle)).unwrap(), JobProgress::Running { .. }));
    let JobProgress::Done(done) = block_on(adapter.poll(&handle)).unwrap() else { panic!("done") };
    assert_eq!(done.outputs, vec![Output::Bytes { mime: "video/mp4".into(), bytes: Arc::from(video) }]);
    assert_eq!(done.usage.cost, Some(nodecraft_types::Money::new(nodecraft_types::Currency::USD, 400_000)));

    let sent = wire.sent();
    let created = body(&sent[0].0);
    assert_eq!(created["duration"], 5);
    assert_eq!(created["generate_audio"], false);
    assert_eq!(created["frame_images"][0]["frame_type"], "first_frame");
    assert!(created["frame_images"][0]["image_url"]["url"].as_str().unwrap().starts_with("data:image/png;base64,"));
    assert_eq!(sent[1].0.url, "https://openrouter.ai/api/v1/videos/job_123");
    assert_eq!(sent[4].0.url, "https://openrouter.ai/api/v1/videos/job_123/content?index=0");
    assert!(sent.iter().take(4).all(|(_, redirects)| !redirects), "API calls never follow redirects");
    assert!(!adapter.retry_ambiguous_submit());
    assert!(block_on(adapter.cancel(&handle)).is_err(), "no cancel endpoint: say so");
}

#[test]
fn video_failures_and_hostile_replies_are_errors_not_panics() {
    let adapter = |replies: Vec<HttpResponse>| OpenRouterVideo::new(Secret::new(KEY).unwrap(), Wire::new(replies));
    let handle = JobHandle { remote_id: "job_1".into(), data: json!({"v": 1, "model": "m/x"}) };
    let failed = adapter(vec![reply(200, json!({"id": "job_1", "status": "failed", "error": {"message": "content policy"}}))]);
    let JobProgress::Failed(error) = block_on(failed.poll(&handle)).unwrap() else { panic!("failed") };
    assert!(error.message.contains("content policy"));
    for hostile in [
        reply(200, json!({"id": "other", "status": "completed"})),
        reply(200, json!({"id": "job_1", "status": "dancing"})),
        reply(200, json!("nope")),
        bytes(200, b"{"),
        reply(500, json!({})),
    ] {
        assert!(block_on(adapter(vec![hostile]).poll(&handle)).is_err());
    }
    let not_video = adapter(vec![reply(200, json!({"id": "job_1", "status": "completed"})), bytes(200, b"not a video")]);
    assert!(block_on(not_video.poll(&handle)).is_err());
    for created in [reply(202, json!({"status": "pending"})), reply(202, json!({"id": "../../x"})), reply(202, json!({"id": KEY})), reply(402, json!({}))] {
        assert!(block_on(adapter(vec![created]).submit(&job("m/x", json!({}), vec![prompt("x")]))).is_err());
    }
    let forged = JobHandle { remote_id: "job_1".into(), data: json!({"v": 1, "model": "m/x", "extra": 1}) };
    assert!(block_on(adapter(vec![]).poll(&forged)).is_err(), "a changed handle is refused before anything is sent");
}

#[test]
fn video_refuses_bad_jobs_before_sending() {
    let wire = Wire::new([]);
    let adapter = OpenRouterVideo::new(Secret::new(KEY).unwrap(), wire.clone());
    let frame = |role: &str| JobInput::Media { role: role.into(), mime: "image/png".into(), bytes: Arc::from(png()) };
    for bad in [
        job("m/x", json!({"fps": 24}), vec![prompt("x")]),
        job("m/x", json!({"duration": -1}), vec![prompt("x")]),
        job("m/x", json!({"duration": 100000}), vec![prompt("x")]),
        job("m/x", json!({"generate_audio": "yes"}), vec![prompt("x")]),
        job("m/x", json!({"resolution": "1080p\"}"}), vec![prompt("x")]),
        job("m/x", json!({}), vec![prompt("x"), frame("first_frame"), frame("first_frame")]),
        job("m/x", json!({}), vec![prompt("x"), frame("reference")]),
        job("m/x", json!({}), vec![prompt("x"), JobInput::Media { role: "first_frame".into(), mime: "image/png".into(), bytes: Arc::from(&b"junk"[..]) }]),
    ] {
        assert_eq!(block_on(adapter.submit(&bad)).expect_err("refused").kind, ErrorKind::UnsupportedInput);
    }
    assert!(wire.sent().is_empty());
}
