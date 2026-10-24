//! Streamed speech previews (#671), through the runner with synthetic chunked replies.
use super::*;
use crate::http::ChunkSink;
use crate::provider::{CancelSignal, Clock};
use crate::{HttpRequest, JobInput, RunFailure, RunLimits, RunUpdate, run_to_completion};
use futures::executor::block_on;
use serde_json::{Value, json};
use std::sync::Mutex;
use std::task::Poll;

const KEY: &str = "fixture-secret-not-real";
const VOICE: &str = "EXAVITQu4vr4xnSDxMaL";

/// An MP3 of `len` bytes: an ID3v2.4 header, one MPEG-1 Layer III frame header, then filler.
pub(super) fn speech(len: usize) -> Vec<u8> {
    let mut bytes = b"ID3\x04\0\0\0\0\0\0\xFF\xFB\x90\x64\0\0\0\0".to_vec();
    bytes.extend((0..len.saturating_sub(bytes.len())).map(|i| (i % 251) as u8));
    bytes
}

/// Answers a streamed request with `chunks` one at a time, letting the runner report between
/// them, then `end` (a cut-off connection) or the end of the reply. A request sent at once gets
/// the chunks whole.
struct Trickle {
    status: u16,
    chunks: Vec<Vec<u8>>,
    end: Mutex<Option<ProviderError>>,
    /// Chunks the adapter took before it stopped the stream.
    taken: Mutex<usize>,
    /// Each request, and whether it was streamed.
    requests: Mutex<Vec<(bool, HttpRequest)>>,
}
impl Trickle {
    fn new(status: u16, body: &[u8], chunk: usize) -> Arc<Self> {
        Self::cut(status, body, chunk, None)
    }
    fn cut(status: u16, body: &[u8], chunk: usize, end: Option<ProviderError>) -> Arc<Self> {
        let chunks = body.chunks(chunk).map(<[u8]>::to_vec).collect();
        Arc::new(Self { status, chunks, end: Mutex::new(end), taken: Mutex::default(), requests: Mutex::default() })
    }
    fn request(&self) -> (bool, HttpRequest) {
        let requests = self.requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "exactly one request");
        requests[0].clone()
    }
}
impl Http for Trickle {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-following fallback forbidden")
    }
    fn send_streaming<'a>(&'a self, _: HttpRequest, _: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        panic!("redirect-following streaming forbidden")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push((false, r));
        let body = self.chunks.concat();
        let status = self.status;
        Box::pin(async move { Ok(HttpResponse { status, headers: vec![], body }) })
    }
    fn send_streaming_no_redirect<'a>(&'a self, r: HttpRequest, on_chunk: &'a mut ChunkSink<'a>) -> BoxFuture<'a, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push((true, r));
        Box::pin(async move {
            if !(200..300).contains(&self.status) {
                return Ok(HttpResponse { status: self.status, headers: vec![], body: self.chunks.concat() });
            }
            for chunk in &self.chunks {
                on_chunk(chunk)?;
                *self.taken.lock().unwrap() += 1;
                yield_once().await;
            }
            match self.end.lock().unwrap().take() {
                Some(e) => Err(e),
                None => Ok(HttpResponse { status: self.status, headers: vec![], body: Vec::new() }),
            }
        })
    }
}

/// Pending once, so whoever polls gets to run.
async fn yield_once() {
    let mut yielded = false;
    futures::future::poll_fn(|cx| {
        if yielded {
            return Poll::Ready(());
        }
        yielded = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    })
    .await;
}

pub(super) struct Never;
impl CancelSignal for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(futures::future::pending())
    }
}
pub(super) struct Still;
impl Clock for Still {
    fn sleep(&self, _ms: u64) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        0
    }
}

pub(super) fn job(model: &str) -> ProviderJob {
    ProviderJob {
        model: model.into(),
        params: json!({"voice": VOICE}).as_object().cloned().unwrap_or_default(),
        inputs: vec![JobInput::Text { role: "prompt".into(), text: "Hello there.".into() }],
        idempotency_key: "fixture-key-no-server-dedup".into(),
    }
}
pub(super) fn previewing() -> RunLimits {
    RunLimits { stream_previews: true, ..RunLimits::default() }
}

fn run(http: &Arc<Trickle>, limits: Limits, run: RunLimits, j: &ProviderJob) -> (Result<Completed, RunFailure>, Vec<RunUpdate>) {
    let p = ElevenLabs::with_limits(Secret::new(KEY).unwrap(), http.clone(), limits).unwrap();
    let mut updates = Vec::new();
    let result = block_on(run_to_completion(&p, j, &Never, &Still, run, |u| updates.push(u)));
    (result, updates)
}

/// The previews reported, in order.
pub(super) fn previews(updates: &[RunUpdate]) -> Vec<Arc<[u8]>> {
    updates
        .iter()
        .map(|u| match u {
            RunUpdate::Progress(JobProgress::Running { fraction: None, message: None, partial: Some(Output::Bytes { mime, bytes }) }) => {
                assert_eq!(mime, "audio/mpeg");
                bytes.clone()
            }
            other => panic!("only previews are reported: {other:?}"),
        })
        .collect()
}

#[test]
fn streamed_speech_is_previewed_as_it_arrives_and_its_result_is_unchanged() {
    let body = speech(300 * 1024);
    let http = Trickle::new(200, &body, 16 * 1024);
    let (streamed, updates) = run(&http, Limits::HARD, previewing(), &job(V3));
    let streamed = streamed.unwrap();
    let (asked, r) = http.request();
    assert!(asked, "speech streams when previews are wanted");
    assert_eq!(r.url, format!("https://api.elevenlabs.io/v1/text-to-speech/{VOICE}/stream?output_format=mp3_44100_128"));
    assert_eq!(r.max_response_bytes, Limits::HARD.output_bytes);
    assert!(r.secret_headers.contains(&"xi-api-key".to_owned()));
    // The first after 64 KiB, then each at twice the audio of the one before.
    let shown = previews(&updates);
    assert_eq!(shown.iter().map(|p| p.len()).collect::<Vec<_>>(), [64 * 1024, 128 * 1024, 256 * 1024]);
    assert!(shown.iter().all(|p| body.starts_with(p)), "each preview is the audio so far");

    // Byte for byte what the same reply sent at once makes, characters billed included.
    let whole = Trickle::new(200, &body, 16 * 1024);
    let (at_once, updates) = run(&whole, Limits::HARD, RunLimits::default(), &job(V3));
    assert!(!whole.request().0);
    assert!(!whole.request().1.url.contains("/stream"));
    assert!(updates.is_empty());
    assert_eq!(streamed, at_once.unwrap());
    let [Output::Bytes { bytes, .. }] = streamed.outputs.as_slice() else { panic!("one MP3") };
    assert_eq!(&bytes[..], &body[..]);
}

#[test]
fn previews_are_few_however_the_reply_is_cut_into_chunks() {
    let body = speech(3 * 1024 * 1024 + 123);
    let http = Trickle::new(200, &body, 1000);
    let (result, updates) = run(&http, Limits::HARD, previewing(), &job(FLASH_V2_5));
    assert!(result.is_ok());
    let lens: Vec<usize> = previews(&updates).iter().map(|p| p.len()).collect();
    assert_eq!(lens.len(), 6, "{lens:?}");
    assert!(lens.len() <= MAX_PREVIEWS);
    assert!(lens[0] >= PREVIEW_FIRST_BYTES && lens.windows(2).all(|w| w[1] >= 2 * w[0]), "{lens:?}");
}

#[test]
fn short_speech_and_audio_that_isnt_mp3_are_never_previewed() {
    let (result, updates) = run(&Trickle::new(200, &speech(60 * 1024), 4096), Limits::HARD, previewing(), &job(V3));
    assert!(result.is_ok());
    assert!(updates.is_empty(), "under 64 KiB there's nothing to preview yet");
    let mut wav = b"RIFF\x24\0\0\0WAVEfmt ".to_vec();
    wav.resize(200 * 1024, 0);
    let (result, updates) = run(&Trickle::new(200, &wav, 16 * 1024), Limits::HARD, previewing(), &job(V3));
    assert_eq!(result.unwrap_err().error.kind, ErrorKind::Malformed);
    assert!(updates.is_empty());
}

#[test]
fn a_streamed_reply_is_capped_like_any_other() {
    let http = Trickle::new(200, &speech(200 * 1024), 16 * 1024);
    let (result, _) = run(&http, Limits { output_bytes: 100_000, ..Limits::HARD }, previewing(), &job(V3));
    let failure = result.unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::Malformed);
    assert_eq!(failure.error.message, "ElevenLabs audio exceeds the size limit");
    assert!(failure.uncertain, "the request was sent, so it may be billed");
    // Six chunks fit under the cap; the stream stops at the seventh.
    assert_eq!(*http.taken.lock().unwrap(), 6);
    assert_eq!(http.request().1.max_response_bytes, 100_000);
}

#[test]
fn a_stream_cut_short_may_be_billed_and_keeps_its_previews() {
    let lost = ProviderError::new(ErrorKind::Network, format!("connection reset; key {KEY}"));
    let http = Trickle::cut(200, &speech(160 * 1024), 16 * 1024, Some(lost));
    let (result, updates) = run(&http, Limits::HARD, previewing(), &job(V3));
    let failure = result.unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::Network);
    assert!(failure.error.message.contains("may have been generated and billed") && !failure.error.message.contains(KEY), "{}", failure.error.message);
    assert!(failure.uncertain);
    assert_eq!(previews(&updates).iter().map(|p| p.len()).collect::<Vec<_>>(), [64 * 1024, 128 * 1024]);
}

#[test]
fn a_refusal_from_the_streaming_endpoint_still_clears_the_charge() {
    let refusal = serde_json::to_vec(&json!({"detail": {"status": "invalid_api_key", "message": "Invalid API key"}})).unwrap();
    let (result, updates) = run(&Trickle::new(401, &refusal, 1024), Limits::HARD, previewing(), &job(V3));
    let failure = result.unwrap_err();
    assert_eq!(failure.error.kind, ErrorKind::Auth);
    assert!(!failure.uncertain, "a documented refusal proves nothing was made");
    assert!(updates.is_empty());
    let (result, _) = run(&Trickle::new(500, b"<html>oops</html>", 1024), Limits::HARD, previewing(), &job(V3));
    assert!(result.unwrap_err().uncertain);
}

#[test]
fn only_speech_streams_and_only_when_previews_are_wanted() {
    let body = speech(100 * 1024);
    for model in [SOUND_EFFECTS, MUSIC] {
        let http = Trickle::new(200, &body, 16 * 1024);
        let (result, updates) = run(&http, Limits::HARD, previewing(), &job(model));
        assert!(result.is_ok() && updates.is_empty(), "{model}");
        assert!(!http.request().0, "{model}");
    }
    // A plain submit, outside the runner, has nowhere to report previews.
    let http = Trickle::new(200, &body, 16 * 1024);
    let p = ElevenLabs::new(Secret::new(KEY).unwrap(), http.clone());
    assert!(block_on(p.submit(&job(V3))).is_ok());
    let (asked, r) = http.request();
    assert!(!asked && !r.url.contains("/stream"));
    let sent: Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(sent["text"], "Hello there.");
}

/// A transport that can't stream: it keeps [`Http::send_streaming_no_redirect`]'s default, which
/// hands the whole reply over as one chunk once it has all arrived.
struct AtOnce(Vec<u8>, Mutex<usize>);
impl Http for AtOnce {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("redirect-following fallback forbidden")
    }
    fn send_no_redirect(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        *self.1.lock().unwrap() += 1;
        let body = self.0.clone();
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) })
    }
}

#[test]
fn a_transport_that_cant_stream_previews_nothing_and_never_the_whole_reply() {
    // #875: a preview equal to the whole reply would play the result before it is one.
    let body = speech(300 * 1024);
    let http = Arc::new(AtOnce(body.clone(), Mutex::default()));
    let p = ElevenLabs::with_limits(Secret::new(KEY).unwrap(), http.clone(), Limits::HARD).unwrap();
    let mut updates = Vec::new();
    let result = block_on(run_to_completion(&p, &job(V3), &Never, &Still, previewing(), |u| updates.push(u))).unwrap();
    assert_eq!(*http.1.lock().unwrap(), 1);
    assert!(previews(&updates).is_empty(), "{} previews", updates.len());
    let [Output::Bytes { bytes, .. }] = result.outputs.as_slice() else { panic!("one MP3") };
    assert_eq!(&bytes[..], &body[..]);
    // A streaming transport never previews its last chunk either: every preview is shorter.
    let (_, updates) = run(&Trickle::new(200, &body, 64 * 1024), Limits::HARD, previewing(), &job(V3));
    let shown = previews(&updates);
    assert!(!shown.is_empty() && shown.iter().all(|p| p.len() < body.len()), "{:?}", shown.iter().map(|p| p.len()).collect::<Vec<_>>());
}
